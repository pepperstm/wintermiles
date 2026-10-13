//! ANT+ device profiles: 8-byte data pages broadcast at a profile-specific rate.

use super::{DeviceKind, Readings, RevCounter, WHEEL_CIRCUMFERENCE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileId {
    Fec,
    Power,
    HeartRate,
    SpeedCadence,
    Cadence,
    Speed,
}

#[derive(Debug, Clone, Copy)]
pub struct AntProfile {
    pub id: ProfileId,
    pub device_type: u8,
    /// Channel period in 1/32768 s.
    pub period: u16,
    pub kind: DeviceKind,
    pub label: &'static str,
}

pub const ANT_PROFILES: [AntProfile; 6] = [
    AntProfile { id: ProfileId::Fec, device_type: 17, period: 8192, kind: DeviceKind::Trainer, label: "Trainer" },
    AntProfile { id: ProfileId::Power, device_type: 11, period: 8182, kind: DeviceKind::Power, label: "Power meter" },
    AntProfile { id: ProfileId::HeartRate, device_type: 120, period: 8070, kind: DeviceKind::Hr, label: "Heart rate" },
    AntProfile { id: ProfileId::SpeedCadence, device_type: 121, period: 8086, kind: DeviceKind::Cadence, label: "Speed & cadence" },
    AntProfile { id: ProfileId::Cadence, device_type: 122, period: 8102, kind: DeviceKind::Cadence, label: "Cadence" },
    AntProfile { id: ProfileId::Speed, device_type: 123, period: 8118, kind: DeviceKind::Speed, label: "Speed" },
];

fn u16_at(p: &[u8; 8], i: usize) -> u16 {
    u16::from_le_bytes([p[i], p[i + 1]])
}

/// Stateful decoder for one channel's data pages.
pub struct PageDecoder {
    profile: ProfileId,
    crank: RevCounter,
    wheel: RevCounter,
    last_event: Option<u8>,
    last_event_at: u64,
    /// (event count, period, accumulated torque) from the previous torque page.
    torque: Option<(u8, u16, u16)>,
    last_power_page_at: Option<u64>,
}

impl PageDecoder {
    pub fn new(profile: ProfileId) -> Self {
        Self {
            profile,
            crank: RevCounter::new(65536, 1024.0),
            wheel: RevCounter::new(65536, 1024.0),
            last_event: None,
            last_event_at: 0,
            torque: None,
            last_power_page_at: None,
        }
    }

    pub fn decode(&mut self, p: &[u8; 8], now_ms: u64) -> Option<Readings> {
        match self.profile {
            // Every HR page, whatever its number, carries the computed heart rate in the last byte.
            ProfileId::HeartRate => Some(Readings { hr: Some(p[7] as f64), ..Default::default() }),
            ProfileId::Fec => decode_fec(p),
            ProfileId::Power => self.decode_power(p, now_ms),
            ProfileId::SpeedCadence => Some(Readings {
                cadence: Some(self.crank.update(u16_at(p, 2) as u64, u16_at(p, 0) as u64, now_ms)),
                wheel_speed: Some(self.wheel.update(u16_at(p, 6) as u64, u16_at(p, 4) as u64, now_ms) / 60.0 * WHEEL_CIRCUMFERENCE),
                ..Default::default()
            }),
            // Single-function sensors put the event time/count in the last four bytes of every page.
            ProfileId::Cadence => Some(Readings { cadence: Some(self.crank.update(u16_at(p, 6) as u64, u16_at(p, 4) as u64, now_ms)), ..Default::default() }),
            ProfileId::Speed => Some(Readings {
                wheel_speed: Some(self.wheel.update(u16_at(p, 6) as u64, u16_at(p, 4) as u64, now_ms) / 60.0 * WHEEL_CIRCUMFERENCE),
                ..Default::default()
            }),
        }
    }

    fn decode_power(&mut self, p: &[u8; 8], now_ms: u64) -> Option<Readings> {
        match p[0] {
            // Standard power-only page.
            0x10 => {
                self.last_power_page_at = Some(now_ms);
                if self.last_event != Some(p[1]) {
                    self.last_event = Some(p[1]);
                    self.last_event_at = now_ms;
                } else if now_ms.saturating_sub(self.last_event_at) > 3000 {
                    // Event-synchronous meters stop counting when the cranks stop.
                    return Some(Readings { power: Some(0.0), cadence: Some(0.0), ..Default::default() });
                }
                Some(Readings { power: Some(u16_at(p, 6) as f64), cadence: (p[3] != 0xff).then_some(p[3] as f64), ..Default::default() })
            }
            // Wheel/crank torque pages; only needed for meters that never send page 0x10.
            0x11 | 0x12 => {
                let cur = (p[1], u16_at(p, 4), u16_at(p, 6));
                let prev = self.torque.replace(cur)?;
                if self.last_power_page_at.is_some_and(|t| now_ms.saturating_sub(t) < 5000) {
                    return None;
                }
                let d_event = cur.0.wrapping_sub(prev.0);
                let d_period = cur.1.wrapping_sub(prev.1);
                let d_torque = cur.2.wrapping_sub(prev.2);
                if d_event == 0 || d_period == 0 {
                    return None;
                }
                // Period in 1/2048 s per revolution, torque in 1/32 Nm: P = 128π·ΔT/ΔP
                Some(Readings {
                    power: Some(128.0 * std::f64::consts::PI * d_torque as f64 / d_period as f64),
                    cadence: (p[0] == 0x12).then(|| d_event as f64 * 2048.0 * 60.0 / d_period as f64),
                    ..Default::default()
                })
            }
            _ => None,
        }
    }
}

/// Decodes the FE-C (smart trainer) data pages; also used for Tacx's FE-C-over-Bluetooth.
pub fn decode_fec(p: &[u8; 8]) -> Option<Readings> {
    match p[0] {
        // General FE data
        0x10 => {
            let speed = u16_at(p, 4);
            Some(Readings {
                wheel_speed: (speed != 0xffff).then_some(speed as f64 * 0.001),
                hr: (p[6] != 0xff && p[6] != 0).then_some(p[6] as f64),
                ..Default::default()
            })
        }
        // Specific trainer data
        0x19 => {
            let power = p[5] as u16 | ((p[6] as u16 & 0x0f) << 8);
            Some(Readings { cadence: (p[2] != 0xff).then_some(p[2] as f64), power: (power != 0xfff).then_some(power as f64), ..Default::default() })
        }
        _ => None,
    }
}

fn scaled(v: f64, lo: f64, hi: f64) -> u16 {
    v.round().clamp(lo, hi) as u16
}

/// FE-C page 51: track resistance (simulated gradient and rolling resistance).
pub fn fec_track_resistance(grade: f64, crr: f64) -> [u8; 8] {
    // Grade in 0.01% units, offset so that 0 = −200%.
    let g = scaled((grade * 100.0 + 200.0) / 0.01, 0.0, 40000.0).to_le_bytes();
    [0x33, 0xff, 0xff, 0xff, 0xff, g[0], g[1], scaled(crr / 0.00005, 0.0, 254.0) as u8]
}

/// FE-C page 50: wind resistance coefficient (kg/m), no wind, no drafting.
pub fn fec_wind_resistance(cw: f64) -> [u8; 8] {
    [0x32, 0xff, 0xff, 0xff, 0xff, scaled(cw / 0.01, 0.0, 186.0) as u8, 127, 100]
}

/// FE-C page 49: target power (ERG mode), 0.25 W units.
pub fn fec_target_power(watts: f64) -> [u8; 8] {
    let w = scaled(watts * 4.0, 0.0, 16000.0).to_le_bytes();
    [0x31, 0xff, 0xff, 0xff, 0xff, 0xff, w[0], w[1]]
}

/// FE-C page 55: user configuration, so the trainer's own simulation uses the right masses.
pub fn fec_user_config(rider_kg: f64, bike_kg: f64) -> [u8; 8] {
    let rider = scaled(rider_kg / 0.01, 0.0, 65534.0).to_le_bytes();
    let bike = scaled(bike_kg / 0.05, 0.0, 4095.0);
    // Byte 4 low nibble = wheel-diameter offset (0xF: unused), high nibble = low 4 bits of bike weight.
    [0x37, rider[0], rider[1], 0xff, (((bike & 0x0f) << 4) | 0x0f) as u8, (bike >> 4) as u8, 70, 0x00]
}

/// The FE-C pages that carry out a trainer command.
pub fn fec_pages(cmd: super::TrainerCommand) -> Vec<[u8; 8]> {
    match cmd {
        super::TrainerCommand::Simulation { grade, crr, cw } => vec![fec_track_resistance(grade, crr), fec_wind_resistance(cw)],
        super::TrainerCommand::TargetPower(w) => vec![fec_target_power(w)],
        super::TrainerCommand::UserConfig { rider_kg, bike_kg } => vec![fec_user_config(rider_kg, bike_kg)],
    }
}
