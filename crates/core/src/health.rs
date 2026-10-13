//! Scanner for Apple Health's `export.xml`. The file can be several GB, so it
//! is scanned line by line as it streams rather than parsed as a document.
//! Each `<Record .../>` opening tag sits on one line.

use crate::types::HealthSummary;

fn attr<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = line.find(&key)? + key.len();
    let end = line[start..].find('"')?;
    Some(&line[start..start + end])
}

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `"2024-01-05 08:12:00 +0100"` → epoch milliseconds.
pub fn parse_health_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 25 {
        return None;
    }
    let num = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let days = days_from_civil(num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let secs = num(11, 13)? * 3600 + num(14, 16)? * 60 + num(17, 19)?;
    let sign = if b[20] == b'-' { -1 } else { 1 };
    let offset = sign * (num(21, 23)? * 3600 + num(23, 25)? * 60);
    Some((days * 86_400 + secs - offset) * 1000)
}

#[derive(Default)]
struct Latest {
    t: i64,
    v: f64,
    date: String,
    seen: bool,
}

impl Latest {
    fn offer(&mut self, t: i64, v: f64, date: &str) {
        if !self.seen || t > self.t {
            *self = Latest { t, v, date: date.to_string(), seen: true };
        }
    }
    fn value(&self) -> Option<f64> {
        self.seen.then_some(self.v)
    }
}

pub struct HealthScanner {
    weight: Latest,
    rhr: Latest,
    vo2: Latest,
    ftp: Latest,
    max_hr: u32,
    dob: String,
    workouts: u32,
    records: u64,
    rest: Vec<u8>,
    /// Only heart rates from the last year count toward the observed max.
    hr_cutoff: i64,
}

impl HealthScanner {
    pub fn new(now_ms: i64) -> Self {
        Self {
            weight: Latest::default(),
            rhr: Latest::default(),
            vo2: Latest::default(),
            ftp: Latest::default(),
            max_hr: 0,
            dob: String::new(),
            workouts: 0,
            records: 0,
            rest: Vec::new(),
            hr_cutoff: now_ms - 365 * 86_400_000,
        }
    }

    /// Feeds the next chunk of the file. Chunks may split lines (and UTF-8 characters) anywhere.
    pub fn push(&mut self, chunk: &[u8]) {
        let mut from = 0;
        while let Some(nl) = chunk[from..].iter().position(|&b| b == b'\n') {
            let end = from + nl;
            if self.rest.is_empty() {
                self.line(&chunk[from..end]);
            } else {
                let mut whole = std::mem::take(&mut self.rest);
                whole.extend_from_slice(&chunk[from..end]);
                self.line(&whole);
            }
            from = end + 1;
        }
        self.rest.extend_from_slice(&chunk[from..]);
    }

    pub fn finish(mut self) -> HealthSummary {
        let rest = std::mem::take(&mut self.rest);
        if !rest.is_empty() {
            self.line(&rest);
        }
        HealthSummary {
            weight_kg: self.weight.value(),
            weight_date: self.weight.seen.then(|| self.weight.date.chars().take(10).collect()),
            resting_hr: self.rhr.value().map(|v| v.round() as u32),
            max_hr_observed: (self.max_hr > 0).then_some(self.max_hr),
            vo2max: self.vo2.value().map(|v| (v * 10.0).round() / 10.0),
            ftp: self.ftp.value().map(|v| v.round() as u32),
            date_of_birth: (!self.dob.is_empty()).then(|| self.dob.clone()),
            cycling_workouts: self.workouts,
            records: self.records,
        }
    }

    fn line(&mut self, bytes: &[u8]) {
        let Ok(line) = std::str::from_utf8(bytes) else { return };
        if line.contains("<Record ") {
            let Some(kind) = attr(line, "type").and_then(|t| t.strip_prefix("HKQuantityTypeIdentifier")) else { return };
            self.records += 1;
            if !matches!(kind, "BodyMass" | "HeartRate" | "RestingHeartRate" | "VO2Max" | "CyclingFunctionalThresholdPower") {
                return;
            }
            let Some(value) = attr(line, "value").and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite()) else { return };
            let date = attr(line, "startDate").unwrap_or("");
            let Some(t) = parse_health_date(date) else { return };
            match kind {
                "HeartRate" => {
                    if t >= self.hr_cutoff && value < 230.0 && value.round() as u32 > self.max_hr {
                        self.max_hr = value.round() as u32;
                    }
                }
                "BodyMass" => {
                    let kg = match attr(line, "unit").unwrap_or("kg") {
                        "lb" => value * 0.453_592_37,
                        "st" => value * 6.350_293_18,
                        "g" => value / 1000.0,
                        _ => value,
                    };
                    self.weight.offer(t, (kg * 10.0).round() / 10.0, date);
                }
                "RestingHeartRate" => self.rhr.offer(t, value, date),
                "VO2Max" => self.vo2.offer(t, value, date),
                _ => self.ftp.offer(t, value, date),
            }
        } else if line.contains("<Workout ") {
            if line.contains("HKWorkoutActivityTypeCycling") {
                self.workouts += 1;
            }
        } else if self.dob.is_empty() && line.contains("<Me ") {
            if let Some(dob) = attr(line, "HKCharacteristicTypeIdentifierDateOfBirth") {
                self.dob = dob.to_string();
            }
        }
    }
}
