//! Decoders/encoders for the standard Bluetooth cycling GATT profiles.
//! All multi-byte fields are little-endian.

pub const SVC_FTMS: u16 = 0x1826;
pub const SVC_CYCLING_POWER: u16 = 0x1818;
pub const SVC_HEART_RATE: u16 = 0x180d;
pub const SVC_CSC: u16 = 0x1816;
pub const CHR_HEART_RATE: u16 = 0x2a37;
pub const CHR_CYCLING_POWER: u16 = 0x2a63;
pub const CHR_CSC: u16 = 0x2a5b;
pub const CHR_INDOOR_BIKE_DATA: u16 = 0x2ad2;
pub const CHR_FTMS_CONTROL_POINT: u16 = 0x2ad9;
/// Tacx trainers that predate FTMS tunnel ANT+ FE-C pages over a Nordic-UART-style service.
pub const SVC_TACX_FEC: u128 = 0x6e40fec1_b5a3_f393_e0a9_e50e24dcca9e;
pub const CHR_TACX_NOTIFY: u128 = 0x6e40fec2_b5a3_f393_e0a9_e50e24dcca9e;
pub const CHR_TACX_WRITE: u128 = 0x6e40fec3_b5a3_f393_e0a9_e50e24dcca9e;

fn u16_at(v: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*v.get(i)?, *v.get(i + 1)?]))
}

fn u32_at(v: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*v.get(i)?, *v.get(i + 1)?, *v.get(i + 2)?, *v.get(i + 3)?]))
}

pub fn parse_heart_rate(v: &[u8]) -> Option<f64> {
    if v.first()? & 0x01 != 0 {
        u16_at(v, 1).map(f64::from)
    } else {
        v.get(1).map(|b| *b as f64)
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct CyclingPower {
    pub power: f64,
    /// (cumulative revolutions, event time in 1/1024 s)
    pub crank: Option<(u64, u64)>,
    /// (cumulative revolutions, event time in 1/2048 s)
    pub wheel: Option<(u64, u64)>,
}

/// Cycling Power Measurement (0x2A63).
pub fn parse_cycling_power(v: &[u8]) -> Option<CyclingPower> {
    let flags = u16_at(v, 0)?;
    let mut out = CyclingPower { power: u16_at(v, 2)? as i16 as f64, ..Default::default() };
    let mut o = 4;
    if flags & 0x01 != 0 {
        o += 1; // pedal power balance
    }
    if flags & 0x04 != 0 {
        o += 2; // accumulated torque
    }
    if flags & 0x10 != 0 {
        out.wheel = Some((u32_at(v, o)? as u64, u16_at(v, o + 4)? as u64));
        o += 6;
    }
    if flags & 0x20 != 0 {
        if let (Some(revs), Some(time)) = (u16_at(v, o), u16_at(v, o + 2)) {
            out.crank = Some((revs as u64, time as u64));
        }
    }
    Some(out)
}

#[derive(Debug, Default, PartialEq)]
pub struct Csc {
    /// (cumulative revolutions, event time in 1/1024 s)
    pub wheel: Option<(u64, u64)>,
    pub crank: Option<(u64, u64)>,
}

/// Cycling Speed and Cadence Measurement (0x2A5B).
pub fn parse_csc(v: &[u8]) -> Option<Csc> {
    let flags = *v.first()?;
    let mut out = Csc::default();
    let mut o = 1;
    if flags & 0x01 != 0 {
        out.wheel = Some((u32_at(v, o)? as u64, u16_at(v, o + 4)? as u64));
        o += 6;
    }
    if flags & 0x02 != 0 {
        out.crank = Some((u16_at(v, o)? as u64, u16_at(v, o + 2)? as u64));
    }
    Some(out)
}

#[derive(Debug, Default, PartialEq)]
pub struct IndoorBikeData {
    /// m/s
    pub speed: Option<f64>,
    pub cadence: Option<f64>,
    pub power: Option<f64>,
    pub hr: Option<f64>,
}

/// FTMS Indoor Bike Data (0x2AD2). Fields are present or absent according to the flag bits.
pub fn parse_indoor_bike_data(v: &[u8]) -> Option<IndoorBikeData> {
    let flags = u16_at(v, 0)?;
    let mut out = IndoorBikeData::default();
    let mut o = 2;
    // Bit 0 is inverted: "more data" clear means instantaneous speed IS present.
    if flags & 0x0001 == 0 {
        out.speed = Some(u16_at(v, o)? as f64 * 0.01 / 3.6);
        o += 2;
    }
    if flags & 0x0002 != 0 {
        o += 2; // average speed
    }
    if flags & 0x0004 != 0 {
        out.cadence = Some(u16_at(v, o)? as f64 * 0.5);
        o += 2;
    }
    if flags & 0x0008 != 0 {
        o += 2; // average cadence
    }
    if flags & 0x0010 != 0 {
        o += 3; // total distance
    }
    if flags & 0x0020 != 0 {
        o += 2; // resistance level
    }
    if flags & 0x0040 != 0 {
        out.power = Some(u16_at(v, o)? as i16 as f64);
        o += 2;
    }
    if flags & 0x0080 != 0 {
        o += 2; // average power
    }
    if flags & 0x0100 != 0 {
        o += 5; // expended energy
    }
    if flags & 0x0200 != 0 {
        out.hr = v.get(o).filter(|hr| **hr > 0).map(|hr| *hr as f64);
    }
    Some(out)
}

pub const FTMS_REQUEST_CONTROL: [u8; 1] = [0x00];
pub const FTMS_START: [u8; 1] = [0x07];

/// FTMS control point: Set Target Power (ERG mode).
pub fn ftms_target_power(watts: f64) -> [u8; 3] {
    let w = (watts.round().clamp(0.0, 4000.0) as i16).to_le_bytes();
    [0x05, w[0], w[1]]
}

/// FTMS control point: Set Indoor Bike Simulation Parameters (no wind).
pub fn ftms_simulation(grade: f64, crr: f64, cw: f64) -> [u8; 7] {
    let g = ((grade * 10_000.0).round().clamp(-4000.0, 4000.0) as i16).to_le_bytes();
    [0x11, 0, 0, g[0], g[1], (crr * 10_000.0).round().clamp(0.0, 255.0) as u8, (cw * 100.0).round().clamp(0.0, 255.0) as u8]
}
