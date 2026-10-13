//! Bluetooth Low Energy sensors and trainers, using the operating system's
//! native stack (CoreBluetooth, WinRT, BlueZ) through btleplug.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use btleplug::api::{bleuuid::uuid_from_u16, Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, ValueNotification, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{mpsc, watch, OnceCell};
use uuid::Uuid;
use wintermiles_core::devices::ant::{encode_message, msg};
use wintermiles_core::devices::ble::*;
use wintermiles_core::devices::profiles::{decode_fec, fec_pages};
use wintermiles_core::devices::{DeviceInfo, DeviceKind, DeviceStatus, Readings, RevCounter, TrainerCommand, Transport, WHEEL_CIRCUMFERENCE};

use super::{Devices, Handle};
use crate::now_ms;

#[derive(Serialize, Clone, PartialEq)]
pub struct Candidate {
    id: String,
    name: String,
}

#[derive(Default)]
pub struct Ble {
    adapter: OnceCell<Adapter>,
    scanning: Arc<AtomicBool>,
}

fn sensor_services() -> [Uuid; 5] {
    [uuid_from_u16(SVC_FTMS), uuid_from_u16(SVC_CYCLING_POWER), uuid_from_u16(SVC_HEART_RATE), uuid_from_u16(SVC_CSC), Uuid::from_u128(SVC_TACX_FEC)]
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Ble {
    async fn adapter(&self) -> Result<&Adapter, String> {
        self.adapter
            .get_or_try_init(|| async {
                let manager = Manager::new().await.map_err(err)?;
                manager.adapters().await.map_err(err)?.into_iter().next().ok_or_else(|| "No Bluetooth adapter found on this computer.".to_string())
            })
            .await
    }

    /// Starts scanning and streams the cycling sensors found to the UI as `ble:candidates`.
    pub async fn scan_start(&self, devices: Arc<Devices>) -> Result<(), String> {
        let adapter = self.adapter().await?.clone();
        // No service filter at the radio level: platforms disagree on whether several
        // services mean "any" or "all". Advertised services are matched below instead.
        adapter.start_scan(ScanFilter::default()).await.map_err(|e| format!("Could not start a Bluetooth scan: {e}. Is Bluetooth switched on and allowed for this app?"))?;
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let scanning = self.scanning.clone();
        tauri::async_runtime::spawn(async move {
            let wanted = sensor_services();
            let mut sent: Vec<Candidate> = Vec::new();
            while scanning.load(Ordering::SeqCst) {
                let mut found = Vec::new();
                for p in adapter.peripherals().await.unwrap_or_default() {
                    let Ok(Some(props)) = p.properties().await else { continue };
                    if !props.services.iter().any(|s| wanted.contains(s)) || devices.contains(&format!("ble:{}", p.id())) {
                        continue;
                    }
                    found.push(Candidate { id: p.id().to_string(), name: props.local_name.unwrap_or_default() });
                }
                if found != sent {
                    let _ = devices.app().emit("ble:candidates", &found);
                    sent = found;
                }
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            let _ = adapter.stop_scan().await;
        });
        Ok(())
    }

    pub fn scan_stop(&self) {
        self.scanning.store(false, Ordering::SeqCst);
    }

    /// Connects to a scanned peripheral and keeps it connected until it is removed.
    pub async fn connect(&self, devices: Arc<Devices>, peripheral_id: &str) -> Result<(), String> {
        let adapter = self.adapter().await?;
        let peripheral = adapter
            .peripherals()
            .await
            .map_err(err)?
            .into_iter()
            .find(|p| p.id().to_string() == peripheral_id)
            .ok_or("That sensor is no longer in range.")?;
        let name = peripheral.properties().await.ok().flatten().and_then(|p| p.local_name).filter(|n| !n.is_empty()).unwrap_or_else(|| "Bluetooth sensor".into());
        let id = format!("ble:{peripheral_id}");

        let (control_tx, control_rx) = mpsc::unbounded_channel::<TrainerCommand>();
        let (stop_tx, stop_rx) = watch::channel(false);
        let info = DeviceInfo { id: id.clone(), name, transport: Transport::Ble, kind: DeviceKind::Hr, status: DeviceStatus::Connecting, controllable: false, readings: Readings::default() };
        let handle = Handle {
            control: Some(Box::new(move |cmd| {
                let _ = control_tx.send(cmd);
            })),
            stop: Box::new(move || {
                let _ = stop_tx.send(true);
            }),
        };
        if !devices.add(info, handle) {
            return Ok(());
        }

        let mut link = Link { peripheral, devices: devices.clone(), id: id.clone(), control_rx, stop_rx };
        let first = match tokio::time::timeout(Duration::from_secs(25), link.open()).await {
            Ok(Ok(session)) => session,
            Ok(Err(e)) => {
                devices.remove(&id);
                let _ = link.peripheral.disconnect().await;
                return Err(e);
            }
            Err(_) => {
                devices.remove(&id);
                let _ = link.peripheral.disconnect().await;
                return Err("Timed out connecting to the sensor.".into());
            }
        };
        tauri::async_runtime::spawn(link.run(first));
        Ok(())
    }
}

/// What a connected peripheral offers, resolved once per connection.
struct Session {
    notifications: BoxStream<'static, ValueNotification>,
    /// Where trainer commands are written, and in which dialect.
    control: Option<(Characteristic, Dialect)>,
    crank: RevCounter,
    wheel: RevCounter,
    power_wheel: RevCounter,
}

#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Ftms,
    TacxFec,
}

struct Link {
    peripheral: Peripheral,
    devices: Arc<Devices>,
    id: String,
    control_rx: mpsc::UnboundedReceiver<TrainerCommand>,
    stop_rx: watch::Receiver<bool>,
}

impl Link {
    /// Connects, discovers what the device is, and subscribes to its measurements.
    async fn open(&mut self) -> Result<Session, String> {
        let p = &self.peripheral;
        if !p.is_connected().await.unwrap_or(false) {
            p.connect().await.map_err(|e| format!("Could not connect: {e}"))?;
        }
        p.discover_services().await.map_err(err)?;
        let chars = p.characteristics();
        let find = |service: Uuid, characteristic: Uuid| chars.iter().find(|c| c.service_uuid == service && c.uuid == characteristic).cloned();

        let ibd = find(uuid_from_u16(SVC_FTMS), uuid_from_u16(CHR_INDOOR_BIKE_DATA));
        let ftms_cp = find(uuid_from_u16(SVC_FTMS), uuid_from_u16(CHR_FTMS_CONTROL_POINT));
        let tacx_rx = find(Uuid::from_u128(SVC_TACX_FEC), Uuid::from_u128(CHR_TACX_NOTIFY));
        let tacx_tx = find(Uuid::from_u128(SVC_TACX_FEC), Uuid::from_u128(CHR_TACX_WRITE));
        let power = find(uuid_from_u16(SVC_CYCLING_POWER), uuid_from_u16(CHR_CYCLING_POWER));
        let hr = find(uuid_from_u16(SVC_HEART_RATE), uuid_from_u16(CHR_HEART_RATE));
        let csc = find(uuid_from_u16(SVC_CSC), uuid_from_u16(CHR_CSC));

        // Tacx's FE-C tunnel is only used when the trainer has no standard FTMS service.
        let tacx_rx = if ibd.is_some() { None } else { tacx_rx };
        let kind = if ibd.is_some() || tacx_rx.is_some() {
            DeviceKind::Trainer
        } else if power.is_some() {
            DeviceKind::Power
        } else if hr.is_some() {
            DeviceKind::Hr
        } else if csc.is_some() {
            DeviceKind::Cadence
        } else {
            return Err("This device has no cycling sensors.".into());
        };

        for c in [&ibd, &tacx_rx, &power, &hr, &csc].into_iter().flatten() {
            p.subscribe(c).await.map_err(|e| format!("Could not subscribe to measurements: {e}"))?;
        }

        let mut control = None;
        if let (Some(cp), true) = (&ftms_cp, ibd.is_some()) {
            // The control point answers by indication; it must be subscribed before any write.
            let ready = async {
                p.subscribe(cp).await?;
                p.write(cp, &FTMS_REQUEST_CONTROL, WriteType::WithResponse).await?;
                // Some trainers reject "start" when already started; that is fine.
                let _ = p.write(cp, &FTMS_START, WriteType::WithResponse).await;
                Ok::<_, btleplug::Error>(())
            };
            match ready.await {
                Ok(()) => control = Some((cp.clone(), Dialect::Ftms)),
                // Read-only FTMS (some spin bikes): still useful as a power/cadence source.
                Err(e) => eprintln!("{}: no FTMS control ({e})", self.id),
            }
        } else if let (Some(tx), true) = (&tacx_tx, tacx_rx.is_some()) {
            control = Some((tx.clone(), Dialect::TacxFec));
        }

        let notifications = p.notifications().await.map_err(err)?;
        let controllable = control.is_some();
        self.devices.update(&self.id, |d| {
            d.kind = kind;
            d.controllable = controllable;
            d.status = DeviceStatus::Connected;
        });
        Ok(Session {
            notifications,
            control,
            crank: RevCounter::new(65536, 1024.0),
            wheel: RevCounter::new(1 << 32, 1024.0),
            power_wheel: RevCounter::new(1 << 32, 2048.0),
        })
    }

    /// Pumps one connection until it drops, then keeps reconnecting until the device is removed.
    async fn run(mut self, first: Session) {
        let mut session = Some(first);
        loop {
            if let Some(s) = session.take() {
                if self.pump(s).await {
                    break;
                }
            }
            self.devices.set_status(&self.id, DeviceStatus::Searching);
            // Sensors drop out when they sleep or the trainer power-cycles; retry until removed.
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                _ = self.stop_rx.changed() => break,
            }
            match tokio::time::timeout(Duration::from_secs(20), self.open()).await {
                Ok(Ok(s)) => session = Some(s),
                _ => {
                    let _ = self.peripheral.disconnect().await;
                }
            }
        }
        let _ = self.peripheral.disconnect().await;
    }

    /// Returns true when the device was removed by the user, false when the link dropped.
    async fn pump(&mut self, mut s: Session) -> bool {
        let mut liveness = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = self.stop_rx.changed() => return true,
                n = s.notifications.next() => match n {
                    Some(n) => self.on_notification(&mut s, n),
                    None => return false,
                },
                cmd = self.control_rx.recv() => {
                    let Some(cmd) = cmd else { return true };
                    // If commands arrive faster than the link carries them, only the
                    // newest of each kind is worth sending.
                    let mut batch = vec![cmd];
                    while let Ok(next) = self.control_rx.try_recv() {
                        batch.retain(|c| std::mem::discriminant(c) != std::mem::discriminant(&next));
                        batch.push(next);
                    }
                    for cmd in batch {
                        self.write(&s.control, cmd).await;
                    }
                }
                // Not every platform ends the notification stream on disconnect.
                _ = liveness.tick() => {
                    if !self.peripheral.is_connected().await.unwrap_or(false) {
                        return false;
                    }
                }
            }
        }
    }

    async fn write(&self, control: &Option<(Characteristic, Dialect)>, cmd: TrainerCommand) {
        let Some((ch, dialect)) = control else { return };
        let write_type = if ch.properties.contains(CharPropFlags::WRITE) { WriteType::WithResponse } else { WriteType::WithoutResponse };
        let packets: Vec<Vec<u8>> = match (dialect, cmd) {
            (Dialect::Ftms, TrainerCommand::Simulation { grade, crr, cw }) => vec![ftms_simulation(grade, crr, cw).to_vec()],
            (Dialect::Ftms, TrainerCommand::TargetPower(w)) => vec![ftms_target_power(w).to_vec()],
            (Dialect::Ftms, TrainerCommand::UserConfig { .. }) => vec![],
            // Each FE-C page travels as a whole ANT "acknowledged data" message on channel 5.
            (Dialect::TacxFec, cmd) => fec_pages(cmd).into_iter().map(|page| encode_message(msg::ACKNOWLEDGED_DATA, &[&[5u8][..], &page[..]].concat())).collect(),
        };
        for packet in packets {
            if let Err(e) = self.peripheral.write(ch, &packet, write_type).await {
                eprintln!("{}: trainer command failed ({e})", self.id);
            }
        }
    }

    fn on_notification(&self, s: &mut Session, n: ValueNotification) {
        let v = &n.value;
        let now = now_ms();
        let readings = if n.uuid == uuid_from_u16(CHR_INDOOR_BIKE_DATA) {
            parse_indoor_bike_data(v).map(|m| Readings { power: m.power, cadence: m.cadence, hr: m.hr, wheel_speed: m.speed })
        } else if n.uuid == uuid_from_u16(CHR_CYCLING_POWER) {
            parse_cycling_power(v).map(|m| Readings {
                power: Some(m.power),
                cadence: m.crank.map(|(revs, time)| s.crank.update(revs, time, now)),
                wheel_speed: m.wheel.map(|(revs, time)| s.power_wheel.update(revs, time, now) / 60.0 * WHEEL_CIRCUMFERENCE),
                hr: None,
            })
        } else if n.uuid == uuid_from_u16(CHR_HEART_RATE) {
            parse_heart_rate(v).map(|hr| Readings { hr: Some(hr), ..Default::default() })
        } else if n.uuid == uuid_from_u16(CHR_CSC) {
            parse_csc(v).map(|m| Readings {
                cadence: m.crank.map(|(revs, time)| s.crank.update(revs, time, now)),
                wheel_speed: m.wheel.map(|(revs, time)| s.wheel.update(revs, time, now) / 60.0 * WHEEL_CIRCUMFERENCE),
                ..Default::default()
            })
        } else if n.uuid == Uuid::from_u128(CHR_TACX_NOTIFY) {
            // Payload is a whole ANT message: SYNC, len, id, channel, 8-byte page, checksum.
            (v.len() >= 12 && v[0] == 0xa4).then(|| <[u8; 8]>::try_from(&v[4..12]).ok()).flatten().and_then(|page| decode_fec(&page))
        } else {
            None
        };
        if let Some(r) = readings {
            self.devices.report(&self.id, r);
        }
    }
}
