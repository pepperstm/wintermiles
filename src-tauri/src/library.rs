//! The rider's library beyond routes: workouts, imported ghosts, Strava segments
//! found along each route, and the season goal.

use chrono::{Datelike, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wintermiles_core::gpx::Route;
use wintermiles_core::ride::GhostTrace;
use wintermiles_core::segments::{ghost_trace, Segment};
use wintermiles_core::types::{Goal, RideSummary};
use wintermiles_core::workout::{builtin_workouts, parse_erg, parse_zwo, Workout};

use crate::store::Store;

// ── Workouts ───────────────────────────────────────────────────────────────

/// A workout as listed in the picker.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkoutMeta {
    id: String,
    name: String,
    description: String,
    duration_s: f64,
    stress: f64,
    built_in: bool,
    /// `[seconds, fraction of FTP]` corner points, for the little profile picture.
    shape: Vec<[f64; 2]>,
}

fn meta(w: &Workout, built_in: bool) -> WorkoutMeta {
    let mut shape = Vec::new();
    let mut t = 0.0;
    for s in &w.steps {
        let (a, b) = match s.kind {
            wintermiles_core::workout::StepKind::Power => (s.from, s.to),
            wintermiles_core::workout::StepKind::Free => (0.6, 0.6),
            wintermiles_core::workout::StepKind::Max => (1.6, 1.6),
        };
        shape.push([t, a]);
        t += s.duration_s;
        shape.push([t, b]);
    }
    WorkoutMeta { id: w.id.clone(), name: w.name.clone(), description: w.description.clone(), duration_s: w.duration(), stress: w.stress(), built_in, shape }
}

pub fn list_workouts(store: &Store) -> Vec<WorkoutMeta> {
    let ids: Vec<String> = store.get("workouts").unwrap_or_default();
    let mine = ids.iter().filter_map(|id| store.get::<Workout>(&format!("workout-{id}"))).map(|w| meta(&w, false));
    mine.chain(builtin_workouts().iter().map(|w| meta(w, true))).collect()
}

pub fn load_workout(store: &Store, id: &str) -> Option<Workout> {
    builtin_workouts().into_iter().find(|w| w.id == id).or_else(|| store.get(&format!("workout-{id}")))
}

/// Reads a .zwo, .erg or .mrc file and adds it to the library.
pub fn import_workout(store: &Store, file_name: &str, text: &str, ftp: f64) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let lower = file_name.to_ascii_lowercase();
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    let workout = if lower.ends_with(".zwo") { parse_zwo(text, &id)? } else { parse_erg(text, &id, stem, ftp)? };
    store.set(&format!("workout-{id}"), &workout)?;
    let mut ids: Vec<String> = store.get("workouts").unwrap_or_default();
    ids.insert(0, id.clone());
    store.set("workouts", &ids)?;
    Ok(workout.name)
}

pub fn delete_workout(store: &Store, id: &str) -> Result<(), String> {
    let mut ids: Vec<String> = store.get("workouts").unwrap_or_default();
    ids.retain(|i| i != id);
    store.set("workouts", &ids)?;
    store.del(&format!("workout-{id}"))
}

// ── Ghosts ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StoredGhost {
    pub id: String,
    pub name: String,
    pub color: u32,
    /// Distance along the route at each second.
    pub trace: Vec<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostMeta {
    id: String,
    name: String,
    color: u32,
    /// How far along the route the ghost goes, and how long it takes.
    distance: f64,
    time_s: f64,
}

const GHOST_COLORS: [u32; 6] = [0x9ad7ff, 0xffe08a, 0xc9a6ff, 0xa6ffcf, 0xffb0c8, 0xd0d0d0];

pub fn list_ghosts(store: &Store, route_id: &str) -> Vec<GhostMeta> {
    let ghosts: Vec<StoredGhost> = store.get(&format!("ghosts-{route_id}")).unwrap_or_default();
    ghosts.iter().map(|g| GhostMeta { id: g.id.clone(), name: g.name.clone(), color: g.color, distance: g.trace.last().copied().unwrap_or(0.0), time_s: g.trace.len().saturating_sub(1) as f64 }).collect()
}

/// Puts a timed track onto the route and keeps it as a ghost.
pub fn add_ghost(store: &Store, route_id: &str, route: &Route, name: &str, track: &[(f64, f64, f64)]) -> Result<GhostMeta, String> {
    let trace = ghost_trace(route, track).ok_or("That ride doesn't start where this route starts, so it can't be raced here.")?;
    if trace.last().copied().unwrap_or(0.0) < 200.0 {
        return Err("That ride only follows the first few metres of this route.".into());
    }
    let key = format!("ghosts-{route_id}");
    let mut ghosts: Vec<StoredGhost> = store.get(&key).unwrap_or_default();
    let ghost = StoredGhost { id: format!("g-{}", uuid::Uuid::new_v4().simple()), name: name.chars().take(40).collect(), color: GHOST_COLORS[ghosts.len() % GHOST_COLORS.len()], trace };
    let m = GhostMeta { id: ghost.id.clone(), name: ghost.name.clone(), color: ghost.color, distance: *ghost.trace.last().unwrap(), time_s: (ghost.trace.len() - 1) as f64 };
    ghosts.push(ghost);
    store.set(&key, &ghosts)?;
    Ok(m)
}

pub fn delete_ghost(store: &Store, route_id: &str, id: &str) -> Result<(), String> {
    let key = format!("ghosts-{route_id}");
    let mut ghosts: Vec<StoredGhost> = store.get(&key).unwrap_or_default();
    ghosts.retain(|g| g.id != id);
    store.set(&key, &ghosts)
}

pub fn load_ghosts(store: &Store, route_id: &str, ids: &[String]) -> Vec<GhostTrace> {
    let ghosts: Vec<StoredGhost> = store.get(&format!("ghosts-{route_id}")).unwrap_or_default();
    ghosts.into_iter().filter(|g| ids.contains(&g.id)).map(|g| GhostTrace { id: g.id, name: g.name, color: g.color, trace: Arc::new(g.trace) }).collect()
}

/// Reads the timed points of a recorded ride from a GPX or FIT file.
pub fn read_track(path: &std::path::Path) -> Result<Vec<(f64, f64, f64)>, String> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".fit") {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let records = fitparser::from_bytes(&bytes).map_err(|e| format!("This FIT file could not be read: {e}"))?;
        const SEMI: f64 = 180.0 / 2_147_483_648.0;
        let points: Vec<(f64, f64, f64)> = records
            .iter()
            .filter(|r| r.kind() == fitparser::profile::MesgNum::Record)
            .filter_map(|r| {
                let field = |name: &str| r.fields().iter().find(|f| f.name() == name).map(|f| f.value().clone());
                let time = match field("timestamp")? {
                    fitparser::Value::Timestamp(t) => t.timestamp() as f64,
                    _ => return None,
                };
                let num = |v: fitparser::Value| -> Option<f64> {
                    match v {
                        fitparser::Value::SInt32(x) => Some(x as f64),
                        fitparser::Value::Float64(x) => Some(x),
                        _ => None,
                    }
                };
                Some((time, num(field("position_lat")?)? * SEMI, num(field("position_long")?)? * SEMI))
            })
            .collect();
        if points.len() < 10 {
            return Err("This FIT file has no GPS track.".into());
        }
        Ok(points)
    } else {
        wintermiles_core::gpx::parse_gpx_track_times(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
    }
}

// ── Segments ───────────────────────────────────────────────────────────────

pub fn route_segments(store: &Store, route_id: &str) -> Vec<Segment> {
    store.get(&format!("segments-{route_id}")).unwrap_or_default()
}

// ── Goal ───────────────────────────────────────────────────────────────────

/// The goal's window: from its start date (default: the start of this winter) to its deadline
/// (default: the next spring equinox, 20 March).
pub fn goal_window(goal: &Goal) -> (NaiveDate, NaiveDate) {
    let today = Local::now().date_naive();
    let season = if today.month() >= 7 { today.year() } else { today.year() - 1 };
    let from = NaiveDate::parse_from_str(&goal.from, "%Y-%m-%d").unwrap_or_else(|_| NaiveDate::from_ymd_opt(season, 9, 1).unwrap());
    let to = NaiveDate::parse_from_str(&goal.deadline, "%Y-%m-%d").unwrap_or_else(|_| NaiveDate::from_ymd_opt(season + 1, 3, 20).unwrap());
    (from, to)
}

/// Fraction of the goal done, counting rides in its window plus `extra_m` metres.
pub fn goal_progress(goal: &Goal, rides: &[RideSummary], extra_m: f64) -> f64 {
    if goal.distance_km <= 0.0 {
        return 0.0;
    }
    let (from, to) = goal_window(goal);
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let (a, b) = ((from - epoch).num_days(), (to - epoch).num_days());
    let done: f64 = rides.iter().filter(|r| (a..=b).contains(&crate::session::local_day(r.started_at))).map(|r| r.distance_m).sum();
    (done + extra_m) / (goal.distance_km * 1000.0)
}
