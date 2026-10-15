//! One ride in progress: owns the simulation, drives the trainer, streams
//! state to the UI and, at the end, writes the recording and updates progress.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{Local, Offset, TimeZone};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wintermiles_core::devices::TrainerCommand;
use wintermiles_core::fit::encode_fit;
use wintermiles_core::fitness::{estimate_ftp, power_curve};
use wintermiles_core::gamify::{apply_ride, streaks, RideFacts};
use wintermiles_core::gpx::{build_route, demo_route, parse_gpx, Climb, Route};
use wintermiles_core::ride::{ClimbResult, RideEngine, RideInput, RideOptions, Rival, RivalKind, SegmentLive, SegmentResult};
use wintermiles_core::segments::{Segment, SegmentSource};
use wintermiles_core::workout::{Workout, WorkoutPos};
use wintermiles_core::types::{PersonalBest, Profile, RideSummary, Settings, TrainerMode};
use wintermiles_core::world::{align_dem_with, route_structures, SceneryCounts, World, WorldLayout, LAYOUT};

use crate::devices::sim::DemoRider;
use crate::devices::Devices;
use crate::store::Store;
use crate::group::GroupLink;
use crate::library;
use crate::smart::SmartLink;
use crate::{mapdata, now_ms, terrain};

pub const DEMO_ROUTE_ID: &str = "demo";
/// Rides shorter than this many seconds aren't worth a file, a history entry or an upload.
const MIN_SAMPLES: usize = 60;
const TICK: Duration = Duration::from_millis(20);
const EMIT_EVERY: Duration = Duration::from_millis(50);

#[derive(serde::Deserialize)]
struct StoredRoute {
    name: String,
    gpx: String,
}

/// What the rider chose before setting off.
#[derive(serde::Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct RideRequest {
    pub route_id: String,
    pub workout_id: Option<String>,
    /// Imported ghosts to race, by id.
    pub ghost_ids: Vec<String>,
    /// Join or host this group-ride room.
    pub group_room: Option<String>,
}

/// Days since the Unix epoch in local time.
pub fn local_day(ms: u64) -> i64 {
    let t = Local.timestamp_millis_opt(ms as i64).single().unwrap_or_else(Local::now);
    (t.date_naive() - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days()
}

pub fn load_route(store: &Store, id: &str) -> Result<Route, String> {
    if id == DEMO_ROUTE_ID {
        return Ok(demo_route());
    }
    let stored: StoredRoute = store.get(&format!("route-{id}")).ok_or("This route is no longer in your library.")?;
    build_route(&stored.name, &parse_gpx(&stored.gpx)?.1)
}

/// Everything the UI needs to set up the ride screen.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RideInit {
    name: String,
    step: f64,
    n: usize,
    total: f64,
    climbs: Vec<Climb>,
    real_terrain: bool,
    /// How many OpenStreetMap buildings were placed along the route.
    buildings: usize,
    /// What other OpenStreetMap scenery the route has.
    scenery: SceneryCounts,
    /// Stretches of the route in a tunnel or under cover, as `[from, to]` in metres.
    covered: Vec<[f64; 2]>,
    layout: WorldLayout,
    rivals: Vec<Rival>,
    pb_climbs: BTreeMap<usize, f64>,
    workout: Option<Workout>,
    segments: Vec<Segment>,
    /// The route's centre, for weather and photos.
    centre: [f64; 2],
    /// Local hour when the ride started (for "real time of day").
    hour: f64,
}

#[derive(Serialize)]
struct RivalState<'a> {
    id: &'a str,
    name: &'a str,
    kind: RivalKind,
    color: u32,
    distance: f64,
    speed: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RideState<'a> {
    distance: f64,
    speed: f64,
    power: f64,
    hr: Option<f64>,
    cadence: Option<f64>,
    elapsed: f64,
    grade: f64,
    ele: f64,
    ascent: f64,
    moving: bool,
    paused: bool,
    finished: bool,
    has_power: bool,
    no_sensors: bool,
    has_trainer: bool,
    lat: f64,
    lon: f64,
    gap: Option<f64>,
    climb_index: Option<usize>,
    climb_elapsed: f64,
    climb_result: Option<&'a ClimbResult>,
    rivals: Vec<RivalState<'a>>,
    demo_target: Option<f64>,
    samples: usize,
    workout: Option<WorkoutPos>,
    workout_time: f64,
    workout_target: Option<f64>,
    intensity: f64,
    cue: Option<&'a str>,
    segment: Option<SegmentLive>,
    segment_result: Option<&'a SegmentResult>,
    group_status: Option<String>,
}

pub struct Session {
    pub route_id: String,
    pub world: Arc<World>,
    engine: Arc<Mutex<RideEngine>>,
    settings: Arc<Mutex<Settings>>,
    stop: Arc<AtomicBool>,
    group: Option<Arc<GroupLink>>,
    imported_ghosts: Vec<String>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Session {
    pub async fn start(
        app: &AppHandle,
        store: &Store,
        http: &reqwest::Client,
        devices: Arc<Devices>,
        demo: Arc<DemoRider>,
        req: &RideRequest,
    ) -> Result<(Arc<Session>, RideInit), String> {
        let route_id = req.route_id.as_str();
        let settings: Settings = store.get("settings").unwrap_or_default();
        let mut route = load_route(store, route_id)?;

        // The demo loop is synthetic, so there is no real place to fetch data for.
        let real_place = route_id != DEMO_ROUTE_ID;
        let want_dem = settings.real_terrain && real_place;
        let (want_buildings, want_scenery) = (settings.buildings && real_place, settings.scenery && real_place);
        if want_dem || want_buildings || want_scenery {
            let _ = app.emit("ride:loading", "Downloading terrain and map data for this area…");
        }
        // Terrain tiles and map data come from different servers; fetch them side by side.
        let (dem, map) = tokio::join!(
            async {
                if want_dem {
                    terrain::load_dem(http, &route).await
                } else {
                    None
                }
            },
            mapdata::load(http, store, route_id, &route, want_buildings, want_scenery)
        );
        // A route with no elevation of its own takes it from the terrain — except on bridges and in tunnels.
        let structures = map.scenery.as_ref().map(|s| route_structures(&route, s)).unwrap_or_default();
        let dem_offset = dem.as_ref().map_or(0.0, |d| align_dem_with(&mut route, d, &structures));
        let _ = app.emit("ride:loading", "Building the world…");

        let pb: Option<PersonalBest> = store.get(&format!("pb-{route_id}"));
        let route = Arc::new(route);
        let real_terrain = dem.is_some();
        let world = Arc::new(World::with_map(route.clone(), dem, dem_offset, map.scenery.as_ref(), &map.footprints));
        if std::env::var_os("WM_SMOKE").is_some() {
            // For the automated check: where along the route the bridges and tunnels are.
            let kinds = world.field.kinds();
            let mut from = 0;
            for i in 1..=kinds.len() {
                if i == kinds.len() || kinds[i] != kinds[from] {
                    if kinds[from] != 0 {
                        eprintln!("{} from {:.0} m to {:.0} m", if kinds[from] == 1 { "Bridge" } else { "Tunnel" }, from as f64 * route.step, (i - 1) as f64 * route.step);
                    }
                    from = i;
                }
            }
            eprintln!("Under cover: {:?}", world.scene.covered);
            let c = world.scene.counts;
            eprintln!("Other roads: {} drawn, {} of their bridges", c.roads, c.road_bridges);
            for deck in &world.scene.decks {
                let mid = deck.line[deck.line.len() / 2];
                let (d, nearest) = (0..route.n).map(|i| ((route.x[i] - mid.0).hypot(route.z[i] - mid.1), i)).min_by(|a, b| a.0.total_cmp(&b.0)).unwrap_or((0.0, 0));
                if d < 60.0 {
                    eprintln!("Bridge on a {:?} {d:.0} m from the route near {:.0} m", deck.kind, nearest as f64 * route.step);
                }
            }
            // How much the roads add to what is on screen at once, at the place the check looks at.
            let at = std::env::var("WM_SMOKE_AT").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let i = ((at / route.step) as usize).min(route.n - 1);
            let (cx, cz) = ((route.x[i] / LAYOUT.chunk).floor() as i32, (route.z[i] / LAYOUT.chunk).floor() as i32);
            let began = std::time::Instant::now();
            let (mut ways, mut most, mut chunks) = (0, 0, 0);
            for dx in -LAYOUT.view_chunks..=LAYOUT.view_chunks {
                for dz in -LAYOUT.view_chunks..=LAYOUT.view_chunks {
                    let n = world.chunk(cx + dx, cz + dz).roads.vertices();
                    ways += n;
                    most = most.max(n);
                    chunks += 1;
                }
            }
            eprintln!("Road vertices in view at {at:.0} m: {ways} in {chunks} chunks (most in one: {most}); all chunks built in {:.0} ms", began.elapsed().as_secs_f64() * 1000.0);
            for over in &world.scene.overbridges {
                let mid = over.line[over.line.len() / 2];
                let nearest = (0..route.n).min_by(|&a, &b| (route.x[a] - mid.0).hypot(route.z[a] - mid.1).total_cmp(&(route.x[b] - mid.0).hypot(route.z[b] - mid.1))).unwrap_or(0);
                eprintln!("Crossed overhead near {:.0} m by a {:.0} m wide {}", nearest as f64 * route.step, over.width, if over.rail { "railway" } else { "road" });
            }
        }
        // Segments: the route's own climbs, plus any Strava segments found along it.
        let mut segments: Vec<Segment> = route
            .climbs
            .iter()
            .enumerate()
            .map(|(i, c)| Segment {
                id: format!("climb-{i}"),
                name: format!("Climb {}{}", i + 1, if c.category.is_empty() { String::new() } else { format!(" · Cat {}", c.category) }),
                source: SegmentSource::Climb,
                start: c.start,
                end: c.end,
                avg_grade: c.avg_grade,
                record: None,
            })
            .collect();
        segments.extend(library::route_segments(store, route_id));
        let workout = match &req.workout_id {
            Some(id) => Some(library::load_workout(store, id).ok_or("That workout is no longer in your library.")?),
            None => None,
        };
        let ghosts = library::load_ghosts(store, route_id, &req.ghost_ids);
        let imported_ghosts = ghosts.iter().map(|g| g.id.clone()).collect();
        let options = RideOptions { workout: workout.clone(), ghosts, segments: segments.clone(), segment_bests: pb.as_ref().map(|p| p.segments.clone()).unwrap_or_default() };
        let mut engine = RideEngine::with_options(route.clone(), &settings, pb.clone(), options);
        // The automated check can start part-way along, to look at a particular spot.
        if let Some(at) = std::env::var("WM_SMOKE_AT").ok().filter(|_| std::env::var_os("WM_SMOKE").is_some()).and_then(|v| v.parse::<f64>().ok()) {
            engine.distance = at.clamp(0.0, route.total - 50.0);
            engine.pos = route.sample_at(engine.distance);
        }
        let init = RideInit {
            name: route.name.clone(),
            step: route.step,
            n: route.n,
            total: route.total,
            climbs: route.climbs.clone(),
            real_terrain,
            buildings: world.buildings.count(),
            scenery: world.scene.counts,
            covered: world.scene.covered.clone(),
            layout: LAYOUT,
            rivals: engine.rivals.clone(),
            pb_climbs: pb.map(|p| p.climbs).unwrap_or_default(),
            workout,
            segments,
            centre: { let m = route.sample_at(route.total / 2.0); [m.lat, m.lon] },
            hour: { use chrono::Timelike; let t = Local::now(); t.hour() as f64 + t.minute() as f64 / 60.0 },
        };
        let group = req.group_room.as_ref().filter(|r| !r.trim().is_empty()).map(|room| GroupLink::join(app.clone(), &settings.group.relay_url, room.trim(), &settings.rider.name, route_id, store));
        devices.command_trainer(TrainerCommand::UserConfig { rider_kg: settings.rider.weight_kg, bike_kg: settings.rider.bike_kg });

        let session = Arc::new(Session {
            route_id: route_id.to_string(),
            world,
            engine: Arc::new(Mutex::new(engine)),
            settings: Arc::new(Mutex::new(settings)),
            stop: Arc::new(AtomicBool::new(false)),
            group,
            imported_ghosts,
            thread: Mutex::new(None),
        });
        let thread = std::thread::spawn({
            let (engine, settings, stop, app, group) = (session.engine.clone(), session.settings.clone(), session.stop.clone(), app.clone(), session.group.clone());
            let smart = SmartLink::start(store.dir(), session.settings.lock().unwrap().smart.clone());
            move || run(engine, settings, stop, app, devices, demo, group, smart)
        });
        *session.thread.lock().unwrap() = Some(thread);
        Ok((session, init))
    }

    pub fn toggle_pause(&self) {
        let mut engine = self.engine.lock().unwrap();
        engine.user_paused = !engine.user_paused;
    }

    /// Applies settings changed mid-ride (trainer mode, ERG target, difficulty).
    pub fn update_settings(&self, settings: Settings) {
        self.engine.lock().unwrap().set_ftp(settings.rider.ftp);
        *self.settings.lock().unwrap() = settings;
    }

    /// Makes the workout harder or easier, in steps of 5%.
    pub fn nudge_intensity(&self, delta: f64) {
        let mut engine = self.engine.lock().unwrap();
        engine.intensity = (engine.intensity + delta).clamp(0.5, 1.5);
    }

    /// Skips to the next workout step.
    pub fn skip_step(&self) {
        let mut engine = self.engine.lock().unwrap();
        if let Some(pos) = engine.workout_pos() {
            engine.workout_time += pos.step_left_s + 0.01;
        }
    }

    /// Stops the simulation loop. Safe to call more than once.
    pub fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(g) = &self.group {
            g.leave();
        }
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }

    /// Ends the ride: writes the FIT file and updates bests, profile and history.
    /// Returns `None` (and saves nothing) for rides too short to keep.
    pub fn finish(&self, store: &Store) -> Result<Option<RideSummary>, String> {
        self.halt();
        let engine = self.engine.lock().unwrap();
        if engine.samples.len() < MIN_SAMPLES {
            return Ok(None);
        }
        let settings = self.settings.lock().unwrap().clone();
        let session = engine.session(now_ms(), Local::now().offset().fix().local_minus_utc() as i64);

        let stamp = Local.timestamp_millis_opt(session.start_time as i64).single().unwrap_or_else(Local::now).format("%Y-%m-%d-%H%M");
        let slug: String = engine.route.name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let slug: String = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-").chars().take(40).collect();
        let file_name = format!("wintermiles-{stamp}-{}.fit", if slug.is_empty() { "ride" } else { &slug });
        let path = store.save_activity(&file_name, &encode_fit(&engine.samples, &session), &settings.export_dir)?;

        let completed = engine.finished;
        let prev = engine.pb.clone().unwrap_or_default();
        let had_time = prev.time_s > 0.0;
        let is_pb = completed && (!had_time || engine.elapsed < prev.time_s);
        let mut climbs = prev.climbs.clone();
        for (i, t) in &engine.climb_times {
            let best = climbs.entry(*i).or_insert(*t);
            *best = best.min(*t);
        }
        let mut segment_bests = prev.segments.clone();
        let mut segment_prs = Vec::new();
        for (id, best) in &engine.new_segment_bests {
            segment_bests.insert(id.clone(), best.clone());
            if let Some(seg) = engine.segments.iter().find(|s| &s.id == id && s.source == SegmentSource::Strava) {
                segment_prs.push(seg.name.clone());
            }
        }
        let pb = if is_pb {
            PersonalBest { time_s: engine.elapsed, trace: engine.trace.iter().map(|d| (d * 10.0).round() / 10.0).collect(), climbs, segments: segment_bests }
        } else {
            PersonalBest { climbs, segments: segment_bests, ..prev }
        };
        store.set(&format!("pb-{}", self.route_id), &pb)?;
        if is_pb {
            let mut bests: BTreeMap<String, f64> = store.get("bests").unwrap_or_default();
            bests.insert(self.route_id.clone(), engine.elapsed);
            store.set("bests", &bests)?;
        }

        let rides: Vec<RideSummary> = store.get("rides").unwrap_or_default();
        let today = local_day(session.start_time);
        let mut days: Vec<i64> = rides.iter().map(|r| local_day(r.started_at)).collect();
        days.push(today);
        let mut routes: Vec<&str> = rides.iter().map(|r| r.route_id.as_str()).chain([self.route_id.as_str()]).collect();
        routes.sort_unstable();
        routes.dedup();
        let goal_before = library::goal_progress(&settings.goal, &rides, 0.0);
        let goal_after = library::goal_progress(&settings.goal, &rides, engine.distance);
        let ramp_test = engine.workout.as_ref().is_some_and(|w| w.id == "builtin-ramp-test");
        let workout_done = engine.workout.is_some() && engine.workout_pos().is_none();
        let curve = power_curve(&engine.samples.iter().map(|s| s.power).collect::<Vec<_>>());
        let ftp_estimate = estimate_ftp(&curve, ramp_test).filter(|e| e.watts > settings.rider.ftp + 2.0 || (ramp_test && workout_done));
        let hour = { use chrono::Timelike; Local.timestamp_millis_opt(session.start_time as i64).single().map_or(12, |t| t.hour()) };
        let beat_other_ghost = completed && engine.rivals.iter().any(|r| r.kind == RivalKind::Ghost && self.imported_ghosts.contains(&r.id) && r.distance < engine.route.total);
        let facts = RideFacts {
            distance_m: engine.distance,
            ascent_m: engine.ascent,
            duration_s: engine.elapsed,
            completed,
            beat_pb: is_pb && had_time,
            np: engine.np(),
            ftp: settings.rider.ftp,
            climb_categories: engine.climb_times.keys().map(|i| engine.route.climbs[*i].category.clone()).collect(),
            workout_done,
            ramp_test,
            beat_other_ghost,
            segment_prs: segment_prs.len(),
            group: self.group.is_some(),
            night: !(5..21).contains(&hour),
            week_streak: streaks(&days, today).weeks,
            routes_ridden: routes.len(),
            goal_reached: goal_before < 1.0 && goal_after >= 1.0,
        };
        let mut profile: Profile = store.get("profile").unwrap_or_default();
        let (xp, new_badges) = apply_ride(&mut profile, &facts);
        store.set("profile", &profile)?;

        let summary = RideSummary {
            id: session.start_time.to_string(),
            route_id: self.route_id.clone(),
            route_name: engine.route.name.clone(),
            started_at: session.start_time,
            duration_s: engine.elapsed,
            distance_m: engine.distance,
            ascent_m: engine.ascent,
            avg_power: session.avg_power,
            max_power: session.max_power,
            np: session.np,
            avg_hr: session.avg_hr,
            max_hr: session.max_hr,
            avg_cadence: session.avg_cadence,
            avg_speed: if engine.elapsed > 0.0 { engine.distance / engine.elapsed } else { 0.0 },
            kj: session.kj,
            tss: engine.tss(),
            completed,
            is_pb,
            xp,
            new_badges,
            file_path: path.to_string_lossy().into_owned(),
            strava_activity_id: None,
            curve,
            workout: engine.workout.as_ref().map(|w| w.name.clone()),
            ftp_estimate,
            segment_prs,
            group: self.group.is_some(),
        };
        let mut rides = rides;
        rides.insert(0, summary.clone());
        rides.truncate(1000);
        store.set("rides", &rides)?;
        Ok(Some(summary))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The simulation loop: 50 Hz physics, 20 Hz state to the UI, 1 Hz commands to the trainer.
#[allow(clippy::too_many_arguments)]
fn run(engine: Arc<Mutex<RideEngine>>, settings: Arc<Mutex<Settings>>, stop: Arc<AtomicBool>, app: AppHandle, devices: Arc<Devices>, demo: Arc<DemoRider>, group: Option<Arc<GroupLink>>, smart: Option<SmartLink>) {
    let mut last = Instant::now();
    let mut emitted = last;
    let mut commanded = last - Duration::from_secs(2);
    // A finished climb's result is shown for a few seconds.
    let mut result_shown: Option<(usize, Instant)> = None;
    let mut segment_shown: Option<(String, Instant)> = None;
    let mut shared = last - Duration::from_secs(2);

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(TICK);
        let now = Instant::now();
        let dt = (now - last).as_secs_f64();
        last = now;
        let (trainer, rider_ftp) = { let s = settings.lock().unwrap(); (s.trainer.clone(), s.rider.ftp) };
        let tel = devices.telemetry(trainer.virtual_power);

        let mut engine = engine.lock().unwrap();
        engine.tick(dt, RideInput { power: tel.power, cadence: tel.cadence, hr: tel.hr }, now_ms());

        if now - commanded >= Duration::from_secs(1) {
            commanded = now;
            let workout_target = engine.workout_target().filter(|_| trainer.workout_erg && !engine.user_paused);
            devices.command_trainer(match (workout_target, trainer.mode) {
                (Some(w), _) => TrainerCommand::TargetPower(w),
                (None, TrainerMode::Erg) if engine.workout.is_none() => TrainerCommand::TargetPower(trainer.erg_watts),
                _ => {
                    let crr = settings.lock().unwrap().physics.crr;
                    TrainerCommand::Simulation { grade: engine.trainer_grade(&trainer), crr, cw: engine.wind_coefficient() }
                }
            });
            if let Some(smart) = &smart {
                smart.update(engine.speed * 3.6, tel.power, tel.hr, rider_ftp);
            }
        }
        if let Some(g) = &group {
            if now - shared >= Duration::from_millis(500) {
                shared = now;
                g.send(engine.distance, engine.speed, tel.power);
            }
            for update in g.take_updates() {
                match update {
                    crate::group::Update::Rider { id, name, color, distance, speed } => engine.set_remote(&id, &name, color, distance, speed),
                    crate::group::Update::Left(id) => engine.remove_remote(&id),
                }
            }
        }

        if now - emitted >= EMIT_EVERY {
            emitted = now;
            if let Some(r) = &engine.last_climb_result {
                if result_shown.is_none_or(|(i, _)| i != r.index) {
                    result_shown = Some((r.index, now));
                }
            }
            let show_result = result_shown.is_some_and(|(_, at)| now - at < Duration::from_secs(9));
            if let Some(r) = &engine.last_segment_result {
                if segment_shown.as_ref().is_none_or(|(id, _)| id != &r.id) {
                    segment_shown = Some((r.id.clone(), now));
                }
            }
            let show_segment = segment_shown.as_ref().is_some_and(|(_, at)| now - *at < Duration::from_secs(9));
            let workout_pos = engine.workout_pos();
            let state = RideState {
                distance: engine.distance,
                speed: engine.speed,
                power: tel.power,
                hr: tel.hr,
                cadence: tel.cadence,
                elapsed: engine.elapsed,
                grade: engine.pos.grade,
                ele: engine.pos.ele,
                ascent: engine.ascent,
                moving: engine.moving,
                paused: engine.user_paused,
                finished: engine.finished,
                has_power: tel.has_power,
                no_sensors: devices.is_empty(),
                has_trainer: devices.has_trainer(),
                lat: engine.pos.lat,
                lon: engine.pos.lon,
                gap: engine.ghost_gap(),
                climb_index: engine.climb_index,
                climb_elapsed: engine.climb_elapsed(),
                climb_result: engine.last_climb_result.as_ref().filter(|_| show_result),
                rivals: engine.rivals.iter().map(|r| RivalState { id: &r.id, name: &r.name, kind: r.kind, color: r.color, distance: r.distance, speed: r.speed }).collect(),
                demo_target: demo.target(&devices),
                samples: engine.samples.len(),
                workout: workout_pos,
                workout_time: engine.workout_time,
                workout_target: workout_pos.map(|p| p.target * rider_ftp * engine.intensity),
                intensity: engine.intensity,
                cue: engine.workout.as_ref().and_then(|w| w.cue_at(engine.workout_time)),
                segment: engine.segment_live(),
                segment_result: engine.last_segment_result.as_ref().filter(|_| show_segment),
                group_status: group.as_ref().map(|g| g.status()),
            };
            let _ = app.emit("ride:state", &state);
        }
    }
    // Leave the trainer on a flat road rather than stuck on the last gradient.
    devices.command_trainer(TrainerCommand::Simulation { grade: 0.0, crr: 0.004, cw: 0.2 });
    if let Some(smart) = smart {
        smart.stop();
    }
}
