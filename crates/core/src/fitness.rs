//! Fitness tracking: best efforts, FTP estimates, power zones and training load.

use serde::{Deserialize, Serialize};

/// Effort lengths (seconds) kept for each ride's power curve.
pub const CURVE_DURATIONS: [u32; 10] = [5, 15, 30, 60, 120, 300, 600, 1200, 2400, 3600];

/// Best average power for each of [`CURVE_DURATIONS`] (0 where the ride was shorter).
pub fn power_curve(watts_per_second: &[f64]) -> Vec<f64> {
    let mut prefix = Vec::with_capacity(watts_per_second.len() + 1);
    prefix.push(0.0);
    for w in watts_per_second {
        prefix.push(prefix.last().unwrap() + w);
    }
    CURVE_DURATIONS
        .iter()
        .map(|&d| {
            let d = d as usize;
            if watts_per_second.len() < d {
                return 0.0;
            }
            (d..prefix.len()).map(|i| prefix[i] - prefix[i - d]).fold(0.0, f64::max) / d as f64
        })
        .collect()
}

/// How an FTP estimate was arrived at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FtpEstimate {
    pub watts: f64,
    pub method: String,
}

/// Estimates FTP from a power curve: the best of 95% of 20-minute power, the
/// 60-minute best, and (for a ramp test) 75% of the best minute.
pub fn estimate_ftp(curve: &[f64], ramp_test: bool) -> Option<FtpEstimate> {
    let at = |d: u32| CURVE_DURATIONS.iter().position(|x| *x == d).and_then(|i| curve.get(i)).copied().unwrap_or(0.0);
    let mut options = vec![(at(1200) * 0.95, "95% of your best 20 minutes"), (at(3600), "your best hour")];
    if ramp_test {
        options.push((at(60) * 0.75, "75% of your best minute in the ramp test"));
    }
    options.into_iter().filter(|(w, _)| *w > 50.0).max_by(|a, b| a.0.total_cmp(&b.0)).map(|(w, m)| FtpEstimate { watts: w.round(), method: m.into() })
}

/// Coggan power zone (1–7) for a power at a given FTP.
pub fn power_zone(power: f64, ftp: f64) -> u8 {
    if ftp <= 0.0 {
        return 1;
    }
    let r = power / ftp;
    [0.55, 0.75, 0.9, 1.05, 1.2, 1.5].iter().take_while(|limit| r >= **limit).count() as u8 + 1
}

/// One day of the training-load chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadDay {
    /// Days since the Unix epoch, in local time.
    pub day: i64,
    pub tss: f64,
    /// Chronic training load ("fitness"), a 42-day exponential average.
    pub fitness: f64,
    /// Acute training load ("fatigue"), a 7-day exponential average.
    pub fatigue: f64,
    /// Fitness minus yesterday's fatigue ("form").
    pub form: f64,
}

/// Builds the daily training-load series from `(day, tss)` pairs, through `today`.
pub fn training_load(rides: &[(i64, f64)], today: i64) -> Vec<LoadDay> {
    let Some(first) = rides.iter().map(|r| r.0).min() else { return Vec::new() };
    let (mut ctl, mut atl) = (0.0f64, 0.0f64);
    let (kc, ka) = (1.0 - (-1.0f64 / 42.0).exp(), 1.0 - (-1.0f64 / 7.0).exp());
    (first..=today.max(first))
        .map(|day| {
            let tss: f64 = rides.iter().filter(|r| r.0 == day).map(|r| r.1).sum();
            let form = ctl - atl;
            ctl += (tss - ctl) * kc;
            atl += (tss - atl) * ka;
            LoadDay { day, tss, fitness: ctl, fatigue: atl, form }
        })
        .collect()
}
