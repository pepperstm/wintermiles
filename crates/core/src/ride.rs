//! The simulation and recorder for one ride.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::fit::{FitSample, FitSession};
use crate::fitness::power_curve;
use crate::gpx::{Climb, Route, RoutePos};
use crate::physics::{step_speed, wind_coefficient, PhysicsParams};
use crate::segments::{Segment, SegmentBest, SegmentSource, SPLIT_M};
use crate::workout::{StepKind, Workout, WorkoutPos};
use crate::types::{PersonalBest, Settings, TrainerSettings};

#[derive(Debug, Clone, Copy, Default)]
pub struct RideInput {
    pub power: f64,
    pub cadence: Option<f64>,
    pub hr: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RivalKind {
    Ghost,
    Bot,
    /// Another rider in a group ride; their position arrives over the network.
    Remote,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rival {
    pub id: String,
    pub name: String,
    pub kind: RivalKind,
    pub color: u32,
    pub distance: f64,
    pub speed: f64,
    /// Bots hold a constant power; the ghost replays a recorded trace.
    #[serde(skip)]
    pub power: f64,
    #[serde(skip)]
    pub mass: f64,
    /// A ghost's distance at each elapsed second.
    #[serde(skip)]
    pub trace: Option<Arc<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentResult {
    pub id: String,
    pub name: String,
    pub source: SegmentSource,
    pub time_s: f64,
    pub previous_best: Option<f64>,
}

/// The segment being ridden, for the live banner.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentLive {
    pub id: String,
    pub name: String,
    pub source: SegmentSource,
    pub to_go: f64,
    pub length: f64,
    pub elapsed: f64,
    /// Seconds behind (+) or ahead (−) of the best effort at this point.
    pub gap: Option<f64>,
    pub best: Option<f64>,
    pub record: Option<String>,
}

/// Someone else's ride on this route, replayed as a ghost.
#[derive(Debug, Clone)]
pub struct GhostTrace {
    pub id: String,
    pub name: String,
    pub color: u32,
    pub trace: Arc<Vec<f64>>,
}

/// Optional extras for a ride: a workout, extra ghosts and segments.
#[derive(Debug, Clone, Default)]
pub struct RideOptions {
    pub workout: Option<Workout>,
    pub ghosts: Vec<GhostTrace>,
    pub segments: Vec<Segment>,
    pub segment_bests: BTreeMap<String, SegmentBest>,
}

#[derive(Debug, Clone)]
enum SegState {
    Waiting,
    Active { entered: f64, splits: Vec<f64> },
    Done,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClimbResult {
    pub index: usize,
    pub climb: Climb,
    pub time_s: f64,
    pub previous_best: Option<f64>,
}

const BOT_NAMES: [&str; 5] = ["Tempo Tom", "Steady Sam", "Punchy Pia", "Diesel Dana", "Rocket Ro"];
const BOT_COLORS: [u32; 5] = [0x4aa3ff, 0xffb347, 0xc56bff, 0x4ddb8a, 0xff6b9a];

pub struct RideEngine {
    pub route: Arc<Route>,
    pub pb: Option<PersonalBest>,
    pub distance: f64,
    pub speed: f64,
    /// Moving time in seconds; the clock stops when the rider does.
    pub elapsed: f64,
    pub moving: bool,
    pub user_paused: bool,
    pub finished: bool,
    pub power: f64,
    pub cadence: Option<f64>,
    pub hr: Option<f64>,
    pub ascent: f64,
    pub descent: f64,
    pub pos: RoutePos,
    pub rivals: Vec<Rival>,
    pub samples: Vec<FitSample>,
    /// Distance at each whole second of moving time — becomes the next ghost.
    pub trace: Vec<f64>,
    pub climb_times: BTreeMap<usize, f64>,
    pub climb_index: Option<usize>,
    pub last_climb_result: Option<ClimbResult>,
    pub segments: Vec<Segment>,
    pub segment_bests: BTreeMap<String, SegmentBest>,
    /// New bests set on this ride, by segment id.
    pub new_segment_bests: BTreeMap<String, SegmentBest>,
    pub last_segment_result: Option<SegmentResult>,
    pub workout: Option<Workout>,
    /// Moving time into the workout.
    pub workout_time: f64,
    /// Scales the workout's targets (1.0 = as written).
    pub intensity: f64,
    seg_state: Vec<SegState>,

    params: PhysicsParams,
    cda: f64,
    ftp: f64,
    acc: f64,
    last_sample_time: u64,
    climb_entered_at: f64,
    n: u32,
    sum_power: f64,
    max_power: f64,
    sum_hr: f64,
    n_hr: u32,
    max_hr: f64,
    sum_cad: f64,
    n_cad: u32,
    max_cad: f64,
    max_speed: f64,
    joules: f64,
    window: Vec<f64>,
    np4: f64,
    n_np: u32,
}

impl RideEngine {
    pub fn new(route: Arc<Route>, settings: &Settings, pb: Option<PersonalBest>) -> Self {
        Self::with_options(route, settings, pb, RideOptions::default())
    }

    pub fn with_options(route: Arc<Route>, settings: &Settings, pb: Option<PersonalBest>, options: RideOptions) -> Self {
        let params = PhysicsParams {
            mass: settings.rider.weight_kg + settings.rider.bike_kg,
            crr: settings.physics.crr,
            cda: settings.physics.cda,
            max_speed: settings.physics.max_speed_kph / 3.6,
        };
        let mut rivals = Vec::new();
        if pb.as_ref().is_some_and(|p| p.trace.len() > 1) {
            let trace = Arc::new(pb.as_ref().unwrap().trace.clone());
            rivals.push(Rival { id: "ghost".into(), name: "Your best".into(), kind: RivalKind::Ghost, color: 0xffffff, distance: 0.0, speed: 0.0, power: 0.0, mass: 0.0, trace: Some(trace) });
        }
        for g in &options.ghosts {
            rivals.push(Rival { id: g.id.clone(), name: g.name.clone(), kind: RivalKind::Ghost, color: g.color, distance: 0.0, speed: 0.0, power: 0.0, mass: 0.0, trace: Some(g.trace.clone()) });
        }
        if settings.bots.enabled {
            for (i, wkg) in settings.bots.wkg.iter().enumerate() {
                rivals.push(Rival {
                    id: format!("bot{i}"),
                    name: format!("{} · {:.1} W/kg", BOT_NAMES[i % BOT_NAMES.len()], wkg),
                    kind: RivalKind::Bot,
                    color: BOT_COLORS[i % BOT_COLORS.len()],
                    distance: 0.0,
                    speed: 0.0,
                    power: wkg * 75.0,
                    mass: 75.0 + 8.5,
                    trace: None,
                });
            }
        }
        let pos = route.sample_at(0.0);
        Self {
            route,
            pb,
            distance: 0.0,
            speed: 0.0,
            elapsed: 0.0,
            moving: false,
            user_paused: false,
            finished: false,
            power: 0.0,
            cadence: None,
            hr: None,
            ascent: 0.0,
            descent: 0.0,
            pos,
            rivals,
            samples: Vec::new(),
            trace: vec![0.0],
            climb_times: BTreeMap::new(),
            climb_index: None,
            last_climb_result: None,
            seg_state: vec![SegState::Waiting; options.segments.len()],
            segments: options.segments,
            segment_bests: options.segment_bests,
            new_segment_bests: BTreeMap::new(),
            last_segment_result: None,
            workout: options.workout,
            workout_time: 0.0,
            intensity: 1.0,
            params,
            cda: settings.physics.cda,
            ftp: settings.rider.ftp,
            acc: 0.0,
            last_sample_time: 0,
            climb_entered_at: 0.0,
            n: 0,
            sum_power: 0.0,
            max_power: 0.0,
            sum_hr: 0.0,
            n_hr: 0,
            max_hr: 0.0,
            sum_cad: 0.0,
            n_cad: 0,
            max_cad: 0.0,
            max_speed: 0.0,
            joules: 0.0,
            window: Vec::with_capacity(31),
            np4: 0.0,
            n_np: 0,
        }
    }

    /// Advances the ride by `dt` seconds. `wall_ms` is the current Unix time, used to stamp samples.
    pub fn tick(&mut self, dt: f64, input: RideInput, wall_ms: u64) {
        self.power = input.power;
        self.cadence = input.cadence;
        self.hr = input.hr;
        if self.finished || self.user_paused {
            self.moving = false;
            return;
        }
        // Hold the bike still until the rider pedals, even if the start points downhill.
        if self.speed < 0.3 && input.power < 5.0 {
            self.speed = 0.0;
            self.moving = false;
            return;
        }
        // After a stall (sleep, debugger) don't teleport down the road.
        let dt = dt.min(0.25);
        self.moving = true;

        let prev_ele = self.pos.ele;
        self.speed = step_speed(self.speed, input.power, self.pos.grade, self.pos.ele, &self.params, dt);
        self.distance += self.speed * dt;
        self.elapsed += dt;
        if self.workout.is_some() {
            self.workout_time += dt;
        }
        if self.distance >= self.route.total {
            self.distance = self.route.total;
            self.finished = true;
        }
        self.pos = self.route.sample_at(self.distance);
        let d_ele = self.pos.ele - prev_ele;
        if d_ele > 0.0 {
            self.ascent += d_ele;
        } else {
            self.descent -= d_ele;
        }
        self.max_speed = self.max_speed.max(self.speed);

        self.step_rivals(dt);
        self.track_climbs();
        self.track_segments();

        self.acc += dt;
        // The epsilon keeps accumulated float error from delaying a sample by a tick.
        while self.acc >= 1.0 - 1e-9 {
            self.acc -= 1.0;
            self.record(wall_ms);
        }
        if self.finished {
            self.record(wall_ms);
        }
    }

    fn step_rivals(&mut self, dt: f64) {
        for r in &mut self.rivals {
            match r.kind {
                RivalKind::Ghost => {
                    let d = r.trace.as_deref().map_or(0.0, |t| trace_at(t, self.elapsed)).min(self.route.total);
                    r.speed = if dt > 0.0 { (d - r.distance) / dt } else { 0.0 };
                    r.distance = d;
                }
                RivalKind::Remote => {}
                RivalKind::Bot if r.distance < self.route.total => {
                    let p = self.route.sample_at(r.distance);
                    let params = PhysicsParams { mass: r.mass, ..self.params };
                    r.speed = step_speed(r.speed, r.power, p.grade, p.ele, &params, dt);
                    r.distance = (r.distance + r.speed * dt).min(self.route.total);
                }
                RivalKind::Bot => r.speed = 0.0,
            }
        }
    }

    fn track_climbs(&mut self) {
        let climbs = &self.route.climbs;
        if let Some(i) = self.climb_index {
            let c = &climbs[i];
            if self.distance >= c.end {
                let time_s = self.elapsed - self.climb_entered_at;
                self.climb_times.insert(i, time_s);
                self.last_climb_result = Some(ClimbResult {
                    index: i,
                    climb: c.clone(),
                    time_s,
                    previous_best: self.pb.as_ref().and_then(|p| p.climbs.get(&i).copied()),
                });
                self.climb_index = None;
            }
            return;
        }
        for (i, c) in climbs.iter().enumerate() {
            if !self.climb_times.contains_key(&i) && self.distance >= c.start && self.distance < c.end {
                self.climb_index = Some(i);
                self.climb_entered_at = self.elapsed;
                return;
            }
        }
    }

    fn record(&mut self, wall_ms: u64) {
        // FIT timestamps must strictly increase even if two samples land in one wall-clock second.
        let time = (self.last_sample_time + 1000).max(wall_ms / 1000 * 1000);
        self.last_sample_time = time;
        let power = self.power.round();
        self.samples.push(FitSample {
            time,
            lat: self.pos.lat,
            lon: self.pos.lon,
            ele: self.pos.ele,
            dist: self.distance,
            speed: self.speed,
            power,
            hr: self.hr,
            cadence: self.cadence,
        });
        self.trace.push(self.distance);
        self.n += 1;
        self.sum_power += power;
        self.joules += power;
        self.max_power = self.max_power.max(power);
        if let Some(hr) = self.hr.filter(|h| *h > 0.0) {
            self.sum_hr += hr;
            self.n_hr += 1;
            self.max_hr = self.max_hr.max(hr);
        }
        if let Some(cad) = self.cadence.filter(|c| *c > 0.0) {
            self.sum_cad += cad;
            self.n_cad += 1;
            self.max_cad = self.max_cad.max(cad);
        }
        // Normalised power: 4th-power mean of the 30 s rolling average.
        self.window.push(power);
        if self.window.len() > 30 {
            self.window.remove(0);
        }
        if self.window.len() == 30 {
            let mean = self.window.iter().sum::<f64>() / 30.0;
            self.np4 += mean.powi(4);
            self.n_np += 1;
        }
    }

    fn track_segments(&mut self) {
        for (i, seg) in self.segments.iter().enumerate() {
            match &mut self.seg_state[i] {
                SegState::Waiting => {
                    // Only time a segment ridden from its start, not one joined part-way.
                    if self.distance >= seg.start && self.distance < seg.end && self.distance - seg.start < 40.0 {
                        self.seg_state[i] = SegState::Active { entered: self.elapsed, splits: vec![0.0] };
                    } else if self.distance >= seg.end {
                        self.seg_state[i] = SegState::Done;
                    }
                }
                SegState::Active { entered, splits } => {
                    let offset = (self.distance - seg.start).min(seg.end - seg.start);
                    while (splits.len() as f64) * SPLIT_M <= offset {
                        splits.push(self.elapsed - *entered);
                    }
                    if self.distance >= seg.end {
                        let time_s = self.elapsed - *entered;
                        splits.push(time_s);
                        let previous_best = self.segment_bests.get(&seg.id).map(|b| b.time_s).filter(|t| *t > 0.0);
                        if previous_best.is_none_or(|b| time_s < b) {
                            self.new_segment_bests.insert(seg.id.clone(), SegmentBest { time_s, splits: std::mem::take(splits) });
                        }
                        self.last_segment_result = Some(SegmentResult { id: seg.id.clone(), name: seg.name.clone(), source: seg.source, time_s, previous_best });
                        self.seg_state[i] = SegState::Done;
                    }
                }
                SegState::Done => {}
            }
        }
    }

    /// The segment being ridden that started most recently.
    pub fn segment_live(&self) -> Option<SegmentLive> {
        let (i, entered) = self
            .seg_state
            .iter()
            .enumerate()
            .filter_map(|(i, s)| if let SegState::Active { entered, .. } = s { Some((i, *entered)) } else { None })
            .max_by(|a, b| a.1.total_cmp(&b.1).then(self.segments[b.0].end.total_cmp(&self.segments[a.0].end)))?;
        let seg = &self.segments[i];
        let elapsed = self.elapsed - entered;
        let best = self.segment_bests.get(&seg.id);
        Some(SegmentLive {
            id: seg.id.clone(),
            name: seg.name.clone(),
            source: seg.source,
            to_go: (seg.end - self.distance).max(0.0),
            length: seg.end - seg.start,
            elapsed,
            gap: best.and_then(|b| b.at(self.distance - seg.start)).map(|t| elapsed - t),
            best: best.map(|b| b.time_s).filter(|t| *t > 0.0),
            record: seg.record.clone(),
        })
    }

    /// Where the workout is now, if one is loaded and not yet over.
    pub fn workout_pos(&self) -> Option<WorkoutPos> {
        self.workout.as_ref()?.at(self.workout_time)
    }

    /// Watts the trainer should hold for the workout, or `None` when the gradient applies.
    pub fn workout_target(&self) -> Option<f64> {
        let pos = self.workout_pos()?;
        (pos.kind == StepKind::Power).then(|| pos.target * self.ftp * self.intensity)
    }

    pub fn set_ftp(&mut self, ftp: f64) {
        self.ftp = ftp;
    }

    /// Best efforts of this ride so far, for the power curve.
    pub fn power_curve(&self) -> Vec<f64> {
        power_curve(&self.samples.iter().map(|s| s.power).collect::<Vec<_>>())
    }

    /// Sets a group-ride partner's position (adds them if new).
    pub fn set_remote(&mut self, id: &str, name: &str, color: u32, distance: f64, speed: f64) {
        let distance = distance.clamp(0.0, self.route.total);
        if let Some(r) = self.rivals.iter_mut().find(|r| r.id == id) {
            r.distance = distance;
            r.speed = speed;
            r.name = name.to_string();
        } else {
            self.rivals.push(Rival { id: id.into(), name: name.into(), kind: RivalKind::Remote, color, distance, speed, power: 0.0, mass: 0.0, trace: None });
        }
    }

    pub fn remove_remote(&mut self, id: &str) {
        self.rivals.retain(|r| !(r.kind == RivalKind::Remote && r.id == id));
    }

    /// Moving time spent on the climb currently being ridden.
    pub fn climb_elapsed(&self) -> f64 {
        if self.climb_index.is_some() {
            self.elapsed - self.climb_entered_at
        } else {
            0.0
        }
    }

    /// Seconds behind (+) or ahead (−) of the personal-best ghost at the current position.
    pub fn ghost_gap(&self) -> Option<f64> {
        let trace = &self.pb.as_ref()?.trace;
        if trace.len() < 2 {
            return None;
        }
        let mut hi = trace.len() - 1;
        if self.distance >= trace[hi] {
            return Some(self.elapsed - hi as f64);
        }
        let mut lo = 0;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if trace[mid] <= self.distance {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let span = trace[hi] - trace[lo];
        let ghost_t = lo as f64 + if span > 0.0 { (self.distance - trace[lo]) / span } else { 0.0 };
        Some(self.elapsed - ghost_t)
    }

    /// Gradient to send to the trainer: scaled, softened downhill and capped as the settings say.
    pub fn trainer_grade(&self, trainer: &TrainerSettings) -> f64 {
        if self.finished || self.user_paused {
            0.0
        } else {
            trainer.trainer_grade(self.pos.grade)
        }
    }

    pub fn wind_coefficient(&self) -> f64 {
        wind_coefficient(self.cda, self.pos.ele)
    }

    pub fn avg_power(&self) -> f64 {
        if self.n > 0 {
            self.sum_power / self.n as f64
        } else {
            0.0
        }
    }

    pub fn np(&self) -> f64 {
        if self.n_np > 0 {
            (self.np4 / self.n_np as f64).powf(0.25)
        } else {
            self.avg_power()
        }
    }

    pub fn kj(&self) -> f64 {
        self.joules / 1000.0
    }

    pub fn tss(&self) -> f64 {
        if self.ftp <= 0.0 || self.elapsed <= 0.0 {
            return 0.0;
        }
        let np = self.np();
        self.elapsed * np * (np / self.ftp) / (self.ftp * 3600.0) * 100.0
    }

    pub fn session(&self, now_ms: u64, utc_offset_s: i64) -> FitSession {
        FitSession {
            start_time: self.samples.first().map_or(now_ms, |s| s.time),
            end_time: self.samples.last().map_or(now_ms, |s| s.time),
            timer_s: self.elapsed,
            distance_m: self.distance,
            ascent_m: self.ascent,
            descent_m: self.descent,
            avg_power: self.avg_power(),
            max_power: self.max_power,
            np: self.np(),
            avg_hr: if self.n_hr > 0 { self.sum_hr / self.n_hr as f64 } else { 0.0 },
            max_hr: self.max_hr,
            avg_cadence: if self.n_cad > 0 { self.sum_cad / self.n_cad as f64 } else { 0.0 },
            max_cadence: self.max_cad,
            max_speed: self.max_speed,
            kj: self.kj(),
            utc_offset_s,
        }
    }
}

/// A recorded trace's distance at `t` seconds, interpolated.
fn trace_at(trace: &[f64], t: f64) -> f64 {
    if trace.is_empty() {
        return 0.0;
    }
    let i = t.max(0.0).floor() as usize;
    if i + 1 >= trace.len() {
        return trace[trace.len() - 1];
    }
    trace[i] + (trace[i + 1] - trace[i]) * (t - i as f64)
}
