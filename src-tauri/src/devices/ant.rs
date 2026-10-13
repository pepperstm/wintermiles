//! ANT+ over a USB stick, driven directly through libusb — no vendor SDK. One
//! wildcard channel is opened per device profile; the first sensor of each
//! kind that the stick hears becomes a device.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusb::{DeviceHandle, GlobalContext};
use wintermiles_core::devices::ant::{encode_message, event, msg, AntMessage, AntParser};
use wintermiles_core::devices::profiles::{fec_pages, AntProfile, PageDecoder, ProfileId, ANT_PROFILES};
use wintermiles_core::devices::{DeviceInfo, DeviceStatus, Readings, TrainerCommand, Transport};

use super::{Devices, Handle};
use crate::now_ms;

const VENDOR: u16 = 0x0fcf;
/// ANTUSB2, ANTUSB-m, ANTUSB1
const PRODUCTS: [u16; 3] = [0x1008, 0x1009, 0x1004];

// The ANT+ managed-network key. It is issued to ANT+ adopters (free
// registration at thisisant.com) and gates access to the ANT+ device profiles.
const ANT_PLUS_NETWORK_KEY: [u8; 8] = [0xb9, 0xa5, 0x21, 0xfb, 0xbd, 0x72, 0xc3, 0x45];
const ANT_PLUS_FREQUENCY: u8 = 57;

enum Cmd {
    Trainer(usize, TrainerCommand),
    CloseChannel(usize),
    Stop,
}

/// Owner of the running stick thread, if any.
#[derive(Default)]
pub struct Ant {
    tx: Mutex<Option<Sender<Cmd>>>,
}

impl Ant {
    /// Opens the stick and starts searching for one sensor of every supported profile.
    pub fn start(&self, devices: Arc<Devices>) {
        let mut slot = self.tx.lock().unwrap();
        if slot.is_some() && matches!(devices.ant_state(), "opening" | "scanning") {
            return;
        }
        devices.set_ant_status("opening", "");
        let (tx, rx) = mpsc::channel();
        *slot = Some(tx.clone());
        std::thread::spawn(move || {
            let outcome = Stick::open().and_then(|usb| {
                let mut runner = Runner { usb, parser: AntParser::default(), channels: Vec::new(), devices: devices.clone(), tx, rx };
                let outcome = runner.setup().and_then(|()| {
                    devices.set_ant_status("scanning", "");
                    runner.run()
                });
                runner.shutdown();
                outcome
            });
            match outcome {
                Ok(()) => devices.set_ant_status("off", ""),
                Err(e) => devices.set_ant_status("error", e),
            }
        });
    }

    pub fn stop(&self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(Cmd::Stop);
        }
    }
}

struct Stick {
    handle: DeviceHandle<GlobalContext>,
    ep_in: u8,
    ep_out: u8,
}

impl Stick {
    fn open() -> Result<Self, String> {
        let handle = PRODUCTS
            .iter()
            .find_map(|pid| rusb::open_device_with_vid_pid(VENDOR, *pid))
            .ok_or("No ANT+ USB stick found (or it could not be opened). Plug one in and try again.")?;
        // On Linux a serial driver may have grabbed the stick; ask libusb to move it aside.
        let _ = handle.set_auto_detach_kernel_driver(true);
        handle.claim_interface(0).map_err(|e| {
            format!("Could not open the ANT+ stick — another app (Garmin Express, Zwift…) may be using it, or the driver/permissions need setting up (see README). [{e}]")
        })?;
        // Bulk endpoints are 0x81/0x01 on every stick in the wild, but read them from the descriptor anyway.
        let (mut ep_in, mut ep_out) = (0x81, 0x01);
        if let Ok(config) = handle.device().active_config_descriptor() {
            for ep in config.interfaces().flat_map(|i| i.descriptors()).flat_map(|d| d.endpoint_descriptors().collect::<Vec<_>>()) {
                if ep.transfer_type() == rusb::TransferType::Bulk {
                    match ep.direction() {
                        rusb::Direction::In => ep_in = ep.address(),
                        rusb::Direction::Out => ep_out = ep.address(),
                    }
                }
            }
        }
        Ok(Self { handle, ep_in, ep_out })
    }

    fn write(&self, id: u8, payload: &[u8]) -> Result<(), String> {
        self.handle.write_bulk(self.ep_out, &encode_message(id, payload), Duration::from_secs(1)).map(|_| ()).map_err(|e| format!("Lost contact with the ANT+ stick ({e})."))
    }

    /// Reads whatever arrives within `timeout`; an empty result just means silence.
    fn read(&self, timeout: Duration) -> Result<Vec<u8>, String> {
        let mut buf = [0u8; 256];
        match self.handle.read_bulk(self.ep_in, &mut buf, timeout) {
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(rusb::Error::Timeout) => Ok(Vec::new()),
            Err(rusb::Error::NoDevice) => Err("The ANT+ stick was unplugged.".into()),
            Err(e) => Err(format!("Lost contact with the ANT+ stick ({e}).")),
        }
    }
}

struct Channel {
    profile: AntProfile,
    decoder: PageDecoder,
    device_id: Option<String>,
    wanted: bool,
    asked_id: bool,
    /// Pages waiting to be sent as acknowledged messages, at most one per page number.
    ack_queue: Vec<[u8; 8]>,
    ack_sent_at: Option<Instant>,
}

struct Runner {
    usb: Stick,
    parser: AntParser,
    channels: Vec<Channel>,
    devices: Arc<Devices>,
    tx: Sender<Cmd>,
    rx: Receiver<Cmd>,
}

impl Runner {
    /// Sends a configuration command and waits for the stick to acknowledge it.
    fn command(&mut self, id: u8, payload: &[u8]) -> Result<(), String> {
        self.usb.write(id, payload)?;
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            let bytes = self.usb.read(Duration::from_millis(50))?;
            for m in self.parser.push(&bytes) {
                if m.id == msg::CHANNEL_EVENT && m.data.len() >= 3 && m.data[0] == payload[0] && m.data[1] == id {
                    return if m.data[2] == 0 { Ok(()) } else { Err(format!("The ANT+ stick rejected command {id:#04x} (code {}).", m.data[2])) };
                }
                self.on_message(m);
            }
        }
        Err(format!("The ANT+ stick did not answer (command {id:#04x})."))
    }

    fn setup(&mut self) -> Result<(), String> {
        self.usb.write(msg::RESET, &[0])?;
        std::thread::sleep(Duration::from_millis(700));
        // Discard the startup notification and anything left over from a previous user.
        while !self.usb.read(Duration::from_millis(30))?.is_empty() {}
        self.parser = AntParser::default();

        self.command(msg::NETWORK_KEY, &[&[0u8][..], &ANT_PLUS_NETWORK_KEY[..]].concat())?;
        // Ask for the transmitting device's ID to be appended to every broadcast.
        let _ = self.command(msg::LIB_CONFIG, &[0, 0x80]);

        for (n, profile) in ANT_PROFILES.iter().enumerate() {
            let ch = n as u8;
            let period = profile.period.to_le_bytes();
            self.channels.push(Channel { profile: *profile, decoder: PageDecoder::new(profile.id), device_id: None, wanted: true, asked_id: false, ack_queue: Vec::new(), ack_sent_at: None });
            self.command(msg::ASSIGN_CHANNEL, &[ch, 0x00, 0])?; // bidirectional slave on network 0
            self.command(msg::CHANNEL_ID, &[ch, 0, 0, profile.device_type, 0])?; // wildcard device number
            self.command(msg::CHANNEL_PERIOD, &[ch, period[0], period[1]])?;
            self.command(msg::CHANNEL_FREQUENCY, &[ch, ANT_PLUS_FREQUENCY])?;
            // Search forever in low-priority mode so the six searches don't starve each
            // other or interrupt channels that are already tracking a sensor.
            let _ = self.command(msg::SEARCH_TIMEOUT, &[ch, 0]);
            let _ = self.command(msg::LOW_PRIORITY_SEARCH_TIMEOUT, &[ch, 255]);
            self.command(msg::OPEN_CHANNEL, &[ch])?;
        }
        Ok(())
    }

    fn run(&mut self) -> Result<(), String> {
        loop {
            let bytes = self.usb.read(Duration::from_millis(100))?;
            for m in self.parser.push(&bytes) {
                self.on_message(m);
            }
            while let Ok(cmd) = self.rx.try_recv() {
                match cmd {
                    Cmd::Stop => return Ok(()),
                    Cmd::CloseChannel(n) => {
                        if let Some(ch) = self.channels.get_mut(n) {
                            ch.wanted = false;
                            ch.device_id = None;
                            let _ = self.usb.write(msg::CLOSE_CHANNEL, &[n as u8]);
                        }
                    }
                    Cmd::Trainer(n, cmd) => {
                        if let Some(ch) = self.channels.get_mut(n) {
                            for page in fec_pages(cmd) {
                                // A newer page with the same number replaces a waiting one.
                                ch.ack_queue.retain(|p| p[0] != page[0]);
                                ch.ack_queue.push(page);
                            }
                        }
                    }
                }
            }
            for n in 0..self.channels.len() {
                self.pump_ack(n);
            }
        }
    }

    /// Sends the next queued acknowledged page once the previous one has been dealt with.
    fn pump_ack(&mut self, n: usize) {
        let ch = &mut self.channels[n];
        // If the stick never reports the outcome, don't block the queue forever.
        if ch.ack_sent_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) || ch.ack_queue.is_empty() {
            return;
        }
        let page = ch.ack_queue.remove(0);
        ch.ack_sent_at = Some(Instant::now());
        let _ = self.usb.write(msg::ACKNOWLEDGED_DATA, &[&[n as u8][..], &page[..]].concat());
    }

    fn on_message(&mut self, m: AntMessage) {
        let d = &m.data;
        if d.is_empty() {
            return;
        }
        let n = d[0] as usize;
        match m.id {
            msg::CHANNEL_EVENT if d.len() >= 3 && d[1] == 1 => self.on_event(n, d[2]),
            msg::BROADCAST_DATA | msg::ACKNOWLEDGED_DATA if d.len() >= 9 => {
                let Some((known, asked)) = self.channels.get(n).filter(|c| c.wanted).map(|c| (c.device_id.is_some(), c.asked_id)) else { return };
                if !known {
                    if d.len() >= 14 && d[9] & 0x80 != 0 {
                        self.attach(n, u16::from_le_bytes([d[10], d[11]]));
                    } else if !asked {
                        // Older sticks without extended messages: ask which device the channel locked onto.
                        self.channels[n].asked_id = true;
                        let _ = self.usb.write(msg::REQUEST, &[n as u8, msg::CHANNEL_ID]);
                    }
                }
                let ch = &mut self.channels[n];
                let Some(id) = &ch.device_id else { return };
                self.devices.set_status(id, DeviceStatus::Connected);
                let page: [u8; 8] = d[1..9].try_into().unwrap();
                if let Some(readings) = ch.decoder.decode(&page, now_ms()) {
                    self.devices.report(id, readings);
                }
            }
            msg::CHANNEL_ID if d.len() >= 3 => {
                if self.channels.get(n).is_some_and(|c| c.wanted && c.device_id.is_none()) {
                    self.attach(n, u16::from_le_bytes([d[1], d[2]]));
                }
            }
            _ => {}
        }
    }

    fn attach(&mut self, n: usize, device_number: u16) {
        let ch = &mut self.channels[n];
        let p = ch.profile;
        let id = format!("ant:{}:{}", p.device_type, device_number);
        let info = DeviceInfo {
            id: id.clone(),
            name: format!("{} {}", p.label, device_number),
            transport: Transport::Ant,
            kind: p.kind,
            status: DeviceStatus::Connected,
            controllable: p.id == ProfileId::Fec,
            readings: Readings::default(),
        };
        let (control_tx, stop_tx) = (self.tx.clone(), self.tx.clone());
        let control: Option<Box<dyn Fn(TrainerCommand) + Send + Sync>> = (p.id == ProfileId::Fec).then(|| {
            Box::new(move |cmd| {
                let _ = control_tx.send(Cmd::Trainer(n, cmd));
            }) as Box<dyn Fn(TrainerCommand) + Send + Sync>
        });
        let handle = Handle {
            control,
            stop: Box::new(move || {
                let _ = stop_tx.send(Cmd::CloseChannel(n));
            }),
        };
        ch.device_id = Some(id);
        self.devices.add(info, handle);
    }

    fn on_event(&mut self, n: usize, code: u8) {
        let Some(ch) = self.channels.get_mut(n) else { return };
        match code {
            event::TRANSFER_TX_COMPLETED | event::TRANSFER_TX_FAILED => ch.ack_sent_at = None,
            event::RX_FAIL_GO_TO_SEARCH => {
                if let Some(id) = &ch.device_id {
                    self.devices.set_status(id, DeviceStatus::Searching);
                }
            }
            event::CHANNEL_CLOSED => {
                // A search timed out (sticks that ignore the infinite timeout): keep looking.
                let next = if ch.wanted { msg::OPEN_CHANNEL } else { msg::UNASSIGN_CHANNEL };
                let _ = self.usb.write(next, &[n as u8]);
            }
            _ => {}
        }
    }

    fn shutdown(&mut self) {
        for ch in &mut self.channels {
            if let Some(id) = ch.device_id.take() {
                self.devices.remove(&id);
            }
        }
        let _ = self.usb.write(msg::RESET, &[0]);
        let _ = self.usb.handle.release_interface(0);
    }
}
