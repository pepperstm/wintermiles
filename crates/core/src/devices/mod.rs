//! Sensor protocols and the hub that merges every connected sensor into one
//! telemetry stream. Transport (Bluetooth radio, USB) lives in the app crate;
//! this module only encodes, decodes and arbitrates.

pub mod ant;
pub mod ble;
pub mod profiles;

use std::collections::HashMap;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Trainer,
    Power,
    Hr,
    Cadence,
    Speed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus {
    Connecting,
    Connected,
    Searching,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Ble,
    Ant,
    Sim,
}

/// One update from a sensor. Fields a sensor doesn't measure stay `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Readings {
    pub power: Option<f64>,
    pub cadence: Option<f64>,
    pub hr: Option<f64>,
    /// m/s
    pub wheel_speed: Option<f64>,
}

/// A command for a controllable trainer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrainerCommand {
    /// Simulate a road: gradient as a fraction, rolling resistance, wind coefficient in kg/m.
    Simulation { grade: f64, crr: f64, cw: f64 },
    /// ERG mode: hold this many watts.
    TargetPower(f64),
    UserConfig { rider_kg: f64, bike_kg: f64 },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub transport: Transport,
    pub kind: DeviceKind,
    pub status: DeviceStatus,
    pub controllable: bool,
    /// Latest fresh values, filled in when the list is reported to the UI.
    pub readings: Readings,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Telemetry {
    pub power: f64,
    pub cadence: Option<f64>,
    pub hr: Option<f64>,
    /// False when nothing connected can measure (or estimate) power.
    pub has_power: bool,
}

#[derive(Clone, Copy)]
enum Metric {
    Power,
    Cadence,
    Hr,
    WheelSpeed,
}

// When several devices report the same metric, the most direct measurement wins:
// a crank power meter beats the trainer's estimate, a chest strap beats a relayed HR.
fn priority(metric: Metric, kind: DeviceKind) -> u8 {
    match (metric, kind) {
        (Metric::Power, DeviceKind::Power) => 3,
        (Metric::Power, DeviceKind::Trainer) => 2,
        (Metric::Cadence, DeviceKind::Cadence) => 3,
        (Metric::Cadence, DeviceKind::Power) => 2,
        (Metric::Cadence, DeviceKind::Trainer) => 1,
        (Metric::Hr, DeviceKind::Hr) => 3,
        (Metric::Hr, DeviceKind::Trainer) => 1,
        _ => 0,
    }
}

const STALE_MS: u64 = 4000;

/// Kurt Kinetic fluid-trainer curve: watts from wheel speed, for "dumb" trainers with only a speed sensor.
pub fn virtual_power(wheel_speed_ms: f64) -> f64 {
    let mph = wheel_speed_ms * 2.236_936;
    5.244_82 * mph + 0.019_168 * mph.powi(3)
}

#[derive(Default)]
struct Latest {
    /// (value, timestamp ms) per metric, indexed by `Metric as usize`.
    values: [Option<(f64, u64)>; 4],
}

/// Registry of connected sensors and the single merged telemetry stream the ride consumes.
#[derive(Default)]
pub struct Hub {
    devices: Vec<DeviceInfo>,
    latest: HashMap<String, Latest>,
}

impl Hub {
    /// Adds a device; returns false if one with the same id is already present.
    pub fn add(&mut self, device: DeviceInfo) -> bool {
        if self.devices.iter().any(|d| d.id == device.id) {
            return false;
        }
        self.devices.push(device);
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| d.id != id);
        self.latest.remove(id);
        self.devices.len() != before
    }

    pub fn contains(&self, id: &str) -> bool {
        self.devices.iter().any(|d| d.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut DeviceInfo> {
        self.devices.iter_mut().find(|d| d.id == id)
    }

    /// Sets a device's status; returns true if it changed.
    pub fn set_status(&mut self, id: &str, status: DeviceStatus) -> bool {
        match self.get_mut(id) {
            Some(d) if d.status != status => {
                d.status = status;
                true
            }
            _ => false,
        }
    }

    pub fn report(&mut self, id: &str, r: Readings, now_ms: u64) {
        if !self.contains(id) {
            return;
        }
        let latest = self.latest.entry(id.to_string()).or_default();
        for (metric, value) in [(Metric::Power, r.power), (Metric::Cadence, r.cadence), (Metric::Hr, r.hr), (Metric::WheelSpeed, r.wheel_speed)] {
            if let Some(v) = value.filter(|v| v.is_finite()) {
                latest.values[metric as usize] = Some((v, now_ms));
            }
        }
    }

    /// The device list with each device's fresh readings attached, for the UI.
    pub fn snapshot(&self, now_ms: u64) -> Vec<DeviceInfo> {
        self.devices
            .iter()
            .map(|d| {
                let fresh = |m: Metric| {
                    self.latest.get(&d.id).and_then(|l| l.values[m as usize]).filter(|(_, t)| now_ms.saturating_sub(*t) < STALE_MS).map(|(v, _)| v)
                };
                DeviceInfo {
                    readings: Readings { power: fresh(Metric::Power), cadence: fresh(Metric::Cadence), hr: fresh(Metric::Hr), wheel_speed: fresh(Metric::WheelSpeed) },
                    ..d.clone()
                }
            })
            .collect()
    }

    fn best(&self, metric: Metric, now_ms: u64) -> Option<f64> {
        let mut best: Option<(u8, f64)> = None;
        for d in &self.devices {
            let Some((v, t)) = self.latest.get(&d.id).and_then(|l| l.values[metric as usize]) else { continue };
            if now_ms.saturating_sub(t) > STALE_MS {
                continue;
            }
            let rank = priority(metric, d.kind);
            if best.is_none_or(|(r, _)| rank > r) {
                best = Some((rank, v));
            }
        }
        best.map(|(_, v)| v)
    }

    pub fn telemetry(&self, use_virtual_power: bool, now_ms: u64) -> Telemetry {
        let mut power = self.best(Metric::Power, now_ms);
        if power.is_none() && use_virtual_power {
            power = self.best(Metric::WheelSpeed, now_ms).map(virtual_power);
        }
        Telemetry { power: power.unwrap_or(0.0).max(0.0), cadence: self.best(Metric::Cadence, now_ms), hr: self.best(Metric::Hr, now_ms), has_power: power.is_some() }
    }

    /// The id of the first connected trainer that accepts commands.
    pub fn trainer(&self) -> Option<&str> {
        self.devices.iter().find(|d| d.controllable && d.status == DeviceStatus::Connected).map(|d| d.id.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

/// Turns the cumulative revolution count + event timestamp pairs that cadence
/// and speed sensors broadcast (BLE and ANT+ alike) into revolutions per minute.
pub struct RevCounter {
    pub rpm: f64,
    rev_modulo: u64,
    /// Timestamp ticks per second.
    time_unit: f64,
    last: Option<(u64, u64)>,
    last_change_ms: u64,
}

impl RevCounter {
    pub fn new(rev_modulo: u64, time_unit: f64) -> Self {
        Self { rpm: 0.0, rev_modulo, time_unit, last: None, last_change_ms: 0 }
    }

    pub fn update(&mut self, revs: u64, time: u64, now_ms: u64) -> f64 {
        match self.last {
            None => self.last_change_ms = now_ms,
            Some((last_revs, last_time)) => {
                let d_revs = (revs + self.rev_modulo - last_revs) % self.rev_modulo;
                let d_time = (time + 65536 - last_time) % 65536;
                if d_revs > 0 && d_time > 0 {
                    // A jump of hundreds of revolutions means the sensor's counter was reset.
                    if d_revs < 200 {
                        self.rpm = d_revs as f64 / (d_time as f64 / self.time_unit) * 60.0;
                    }
                    self.last_change_ms = now_ms;
                } else if now_ms.saturating_sub(self.last_change_ms) > 2500 {
                    // Sensors repeat their last event while stationary.
                    self.rpm = 0.0;
                }
            }
        }
        self.last = Some((revs, time));
        self.rpm
    }
}

/// Default 700x25c wheel circumference in metres, used to turn wheel rpm into speed.
pub const WHEEL_CIRCUMFERENCE: f64 = 2.105;
