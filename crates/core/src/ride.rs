//! The simulation and recorder for one ride.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::fit::{FitSample, FitSession};
use crate::gpx::{Climb, Route, RoutePos};
use crate::physics::{step_speed, wind_coefficient, PhysicsParams};
use crate::types::{PersonalBest, Settings};

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
        let params = PhysicsParams {
            mass: settings.rider.weight_kg + settings.rider.bike_kg,
            crr: settings.physics.crr,
            cda: settings.physics.cda,
            max_speed: settings.physics.max_speed_kph / 3.6,
        };
        let mut rivals = Vec::new();
        if pb.as_ref().is_some_and(|p| p.trace.len() > 1) {
            rivals.push(Rival { id: "ghost".into(), name: "Your best".into(), kind: RivalKind::Ghost, color: 0xffffff, distance: 0.0, speed: 0.0, power: 0.0, mass: 0.0 });
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
        let ghost_distance = self.trace_at(self.elapsed);
        for r in &mut self.rivals {
            match r.kind {
                RivalKind::Ghost => {
                    r.speed = if dt > 0.0 { (ghost_distance - r.distance) / dt } else { 0.0 };
                    r.distance = ghost_distance;
                }
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

    fn trace_at(&self, t: f64) -> f64 {
        let Some(trace) = self.pb.as_ref().map(|p| &p.trace).filter(|t| !t.is_empty()) else { return 0.0 };
        let i = t.floor() as usize;
        if i + 1 >= trace.len() {
            return trace[trace.len() - 1];
        }
        trace[i] + (trace[i + 1] - trace[i]) * (t - i as f64)
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

    /// Gradient to send to the trainer, scaled by the "trainer difficulty" (0–1).
    pub fn trainer_grade(&self, difficulty: f64) -> f64 {
        if self.finished || self.user_paused {
            0.0
        } else {
            self.pos.grade * difficulty
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
