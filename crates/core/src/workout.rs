//! Structured workouts: a list of power targets over time, ridden in ERG mode
//! while the route scrolls past. Reads Zwift `.zwo` files and the older
//! `.erg` / `.mrc` text formats; targets are kept as fractions of FTP.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// Power moves linearly from `from` to `to` (equal for a steady block).
    Power,
    /// No target: the trainer follows the gradient.
    Free,
    /// All-out: no target, ridden on the gradient.
    Max,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub kind: StepKind,
    pub duration_s: f64,
    /// Target at the start of the step, as a fraction of FTP.
    pub from: f64,
    /// Target at the end of the step, as a fraction of FTP.
    pub to: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence: Option<f64>,
}

/// A message shown on screen at a given time into the workout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    pub at_s: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workout {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub cues: Vec<Cue>,
}

/// Where in a workout the rider is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkoutPos {
    pub index: usize,
    pub kind: StepKind,
    /// Target now, as a fraction of FTP (0 for free and max-effort steps).
    pub target: f64,
    pub step_left_s: f64,
    pub cadence: Option<f64>,
}

impl Workout {
    pub fn duration(&self) -> f64 {
        self.steps.iter().map(|s| s.duration_s).sum()
    }

    /// The step and target at `t` seconds in; `None` once the workout is over.
    pub fn at(&self, t: f64) -> Option<WorkoutPos> {
        let mut start = 0.0;
        for (index, s) in self.steps.iter().enumerate() {
            if t < start + s.duration_s {
                let f = ((t - start) / s.duration_s).clamp(0.0, 1.0);
                let target = if s.kind == StepKind::Power { s.from + (s.to - s.from) * f } else { 0.0 };
                return Some(WorkoutPos { index, kind: s.kind, target, step_left_s: start + s.duration_s - t, cadence: s.cadence });
            }
            start += s.duration_s;
        }
        None
    }

    /// The cue to show at `t`: each stays up for ten seconds.
    pub fn cue_at(&self, t: f64) -> Option<&str> {
        self.cues.iter().rev().find(|c| t >= c.at_s && t < c.at_s + 10.0).map(|c| c.text.as_str())
    }

    /// Training stress of the whole workout at the given FTP, counting free steps as endurance pace.
    pub fn stress(&self) -> f64 {
        let mut sum4 = 0.0;
        for s in &self.steps {
            // Average of the fourth power across a linear ramp.
            let (a, b) = match s.kind {
                StepKind::Power => (s.from, s.to),
                StepKind::Free => (0.6, 0.6),
                StepKind::Max => (1.5, 1.5),
            };
            let mean4 = if (a - b).abs() < 1e-9 { a.powi(4) } else { (b.powi(5) - a.powi(5)) / (5.0 * (b - a)) };
            sum4 += mean4 * s.duration_s;
        }
        let d = self.duration();
        if d <= 0.0 {
            return 0.0;
        }
        let intensity = (sum4 / d).powf(0.25);
        d * intensity * intensity / 3600.0 * 100.0
    }
}

fn attr(n: &roxmltree::Node, names: &[&str]) -> Option<f64> {
    // ZWO attribute names vary in case between tools.
    n.attributes().find(|a| names.iter().any(|name| a.name().eq_ignore_ascii_case(name))).and_then(|a| a.value().trim().parse::<f64>().ok()).filter(|v| v.is_finite())
}

/// Reads a Zwift workout file.
pub fn parse_zwo(text: &str, id: &str) -> Result<Workout, String> {
    let doc = roxmltree::Document::parse(text.trim_start_matches('\u{feff}')).map_err(|e| format!("This is not a valid .zwo workout ({e})."))?;
    let child_text = |tag: &str| doc.descendants().find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(tag)).and_then(|n| n.text()).map(|t| t.trim().to_string()).unwrap_or_default();
    let body = doc.descendants().find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case("workout")).ok_or("No <workout> section in this file.")?;
    let mut steps = Vec::new();
    let mut cues = Vec::new();
    let mut t = 0.0;
    for n in body.children().filter(|n| n.is_element()) {
        let dur = attr(&n, &["Duration"]).unwrap_or(0.0);
        let cadence = attr(&n, &["Cadence"]);
        let low = attr(&n, &["PowerLow"]);
        let high = attr(&n, &["PowerHigh"]);
        let begun = t;
        match n.tag_name().name().to_ascii_lowercase().as_str() {
            "steadystate" | "steady" => {
                let p = attr(&n, &["Power"]).or(low).unwrap_or(0.6);
                steps.push(Step { kind: StepKind::Power, duration_s: dur, from: p, to: p, cadence });
            }
            "warmup" => {
                let (a, b) = (low.unwrap_or(0.4), high.unwrap_or(0.75));
                steps.push(Step { kind: StepKind::Power, duration_s: dur, from: a.min(b), to: a.max(b), cadence });
            }
            "cooldown" => {
                let (a, b) = (low.unwrap_or(0.75), high.unwrap_or(0.4));
                steps.push(Step { kind: StepKind::Power, duration_s: dur, from: a.max(b), to: a.min(b), cadence });
            }
            "ramp" => steps.push(Step { kind: StepKind::Power, duration_s: dur, from: low.unwrap_or(0.5), to: high.unwrap_or(0.8), cadence }),
            "intervalst" => {
                let repeat = attr(&n, &["Repeat"]).unwrap_or(1.0).max(1.0) as usize;
                let (on, off) = (attr(&n, &["OnDuration"]).unwrap_or(60.0), attr(&n, &["OffDuration"]).unwrap_or(60.0));
                let on_p = attr(&n, &["OnPower", "PowerOnLow"]).unwrap_or(1.0);
                let on_q = attr(&n, &["PowerOnHigh"]).unwrap_or(on_p);
                let off_p = attr(&n, &["OffPower", "PowerOffLow"]).unwrap_or(0.5);
                let off_q = attr(&n, &["PowerOffHigh"]).unwrap_or(off_p);
                let (on_cad, off_cad) = (attr(&n, &["Cadence"]), attr(&n, &["CadenceResting"]));
                for _ in 0..repeat {
                    steps.push(Step { kind: StepKind::Power, duration_s: on, from: on_p, to: on_q, cadence: on_cad });
                    steps.push(Step { kind: StepKind::Power, duration_s: off, from: off_p, to: off_q, cadence: off_cad });
                }
            }
            "freeride" => steps.push(Step { kind: StepKind::Free, duration_s: dur, from: 0.0, to: 0.0, cadence }),
            "maxeffort" => steps.push(Step { kind: StepKind::Max, duration_s: dur, from: 0.0, to: 0.0, cadence }),
            _ => continue,
        }
        for ev in n.children().filter(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case("textevent")) {
            let msg = ev.attributes().find(|a| a.name().eq_ignore_ascii_case("message")).map(|a| a.value().trim().to_string()).unwrap_or_default();
            if !msg.is_empty() {
                cues.push(Cue { at_s: begun + attr(&ev, &["timeoffset"]).unwrap_or(0.0), text: msg });
            }
        }
        t = steps.iter().map(|s| s.duration_s).sum();
    }
    steps.retain(|s| s.duration_s > 0.0);
    if steps.is_empty() {
        return Err("This workout has no steps.".into());
    }
    cues.sort_by(|a, b| a.at_s.total_cmp(&b.at_s));
    let name = child_text("name");
    Ok(Workout { id: id.into(), name: if name.is_empty() { "Workout".into() } else { name }, description: child_text("description"), steps, cues })
}

/// Reads an `.erg` (minutes and watts) or `.mrc` (minutes and percent of FTP) file.
/// Absolute watts are converted to fractions of `ftp`.
pub fn parse_erg(text: &str, id: &str, fallback_name: &str, ftp: f64) -> Result<Workout, String> {
    let mut percent = false;
    let mut name = String::new();
    let mut description = String::new();
    let mut in_data = false;
    let mut points: Vec<(f64, f64)> = Vec::new();
    for line in text.lines().map(str::trim) {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("[COURSE DATA]") {
            in_data = true;
            continue;
        }
        if upper.starts_with("[END COURSE DATA]") {
            in_data = false;
            continue;
        }
        if !in_data {
            if let Some((k, v)) = line.split_once('=') {
                match k.trim().to_ascii_uppercase().as_str() {
                    "FILE NAME" => name = v.trim().trim_end_matches(".erg").trim_end_matches(".mrc").to_string(),
                    "DESCRIPTION" => description = v.trim().to_string(),
                    _ => {}
                }
            } else if upper.contains("PERCENT") {
                percent = true;
            }
            continue;
        }
        let nums: Vec<f64> = line.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        if nums.len() >= 2 && nums[0].is_finite() && nums[1].is_finite() {
            points.push((nums[0] * 60.0, nums[1]));
        }
    }
    if points.len() < 2 {
        return Err("No course data found in this file.".into());
    }
    if !percent && ftp <= 0.0 {
        return Err("Set your FTP before importing a workout in watts.".into());
    }
    let frac = |v: f64| if percent { v / 100.0 } else { v / ftp };
    let steps: Vec<Step> = points
        .windows(2)
        .filter(|w| w[1].0 > w[0].0)
        .map(|w| Step { kind: StepKind::Power, duration_s: w[1].0 - w[0].0, from: frac(w[0].1), to: frac(w[1].1), cadence: None })
        .collect();
    if steps.is_empty() {
        return Err("This workout has no steps.".into());
    }
    Ok(Workout { id: id.into(), name: if name.is_empty() { fallback_name.into() } else { name }, description, steps, cues: Vec::new() })
}

fn steady(minutes: f64, p: f64) -> Step {
    Step { kind: StepKind::Power, duration_s: minutes * 60.0, from: p, to: p, cadence: None }
}

fn ramp(minutes: f64, a: f64, b: f64) -> Step {
    Step { kind: StepKind::Power, duration_s: minutes * 60.0, from: a, to: b, cadence: None }
}

fn repeat(n: usize, on: Step, off: Step) -> Vec<Step> {
    (0..n).flat_map(|_| [on.clone(), off.clone()]).collect()
}

fn built(id: &str, name: &str, description: &str, parts: Vec<Vec<Step>>, cues: Vec<Cue>) -> Workout {
    Workout { id: id.into(), name: name.into(), description: description.into(), steps: parts.into_iter().flatten().collect(), cues }
}

/// The ramp test's step: one minute per rung, each 6.25% of FTP harder (about 20 W at 320 W).
pub const RAMP_RUNG_S: f64 = 60.0;

/// Workouts that ship with the app.
pub fn builtin_workouts() -> Vec<Workout> {
    let warm = vec![ramp(10.0, 0.45, 0.75)];
    let cool = vec![ramp(8.0, 0.65, 0.4)];
    vec![
        built("builtin-endurance", "Endurance hour", "Steady zone 2 riding: the base of winter training.", vec![warm.clone(), vec![steady(42.0, 0.68)], cool.clone()], vec![]),
        built(
            "builtin-sweetspot",
            "Sweet spot 3 × 10",
            "Three ten-minute blocks just under threshold, with five minutes easy between.",
            vec![warm.clone(), repeat(3, steady(10.0, 0.9), steady(5.0, 0.55)), cool.clone()],
            vec![Cue { at_s: 600.0, text: "First sweet-spot block: smooth and steady.".into() }],
        ),
        built(
            "builtin-threshold",
            "Threshold 2 × 20",
            "The classic: two twenty-minute efforts at FTP.",
            vec![warm.clone(), vec![steady(20.0, 1.0), steady(8.0, 0.5)], vec![steady(20.0, 1.0)], cool.clone()],
            vec![],
        ),
        built("builtin-vo2", "VO₂max 5 × 3", "Five hard three-minute efforts with equal recovery.", vec![warm.clone(), repeat(5, steady(3.0, 1.18), steady(3.0, 0.5)), cool.clone()], vec![]),
        built(
            "builtin-overunder",
            "Over-unders 4 × 8",
            "Two minutes just under FTP, one minute just over, repeated within each block.",
            vec![warm.clone(), (0..4).flat_map(|_| [repeat(2, steady(2.0, 0.95), steady(1.0, 1.08)), vec![steady(2.0, 0.95), steady(4.0, 0.5)]].concat()).collect(), cool.clone()],
            vec![],
        ),
        built(
            "builtin-sprints",
            "Sprints and spin-ups",
            "Endurance riding with eight short all-out sprints.",
            vec![warm.clone(), repeat(8, Step { kind: StepKind::Max, duration_s: 15.0, from: 0.0, to: 0.0, cadence: Some(110.0) }, steady(3.75, 0.6)), cool.clone()],
            vec![],
        ),
        Workout {
            id: "builtin-ramp-test".into(),
            name: "FTP ramp test".into(),
            description: "Warm up, then the target climbs every minute until you can't hold it. Your FTP is estimated as 75% of your best one-minute power.".into(),
            steps: [vec![steady(5.0, 0.4)], (0..28).map(|i| Step { kind: StepKind::Power, duration_s: RAMP_RUNG_S, from: 0.4 + 0.0625 * i as f64, to: 0.4 + 0.0625 * i as f64, cadence: None }).collect()].concat(),
            cues: vec![Cue { at_s: 300.0, text: "The ramp begins. Ride until you can't hold the target, then end the ride.".into() }],
        },
    ]
}
