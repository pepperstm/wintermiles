//! Data that is persisted as JSON and shared with the UI. Field names are
//! camelCase on the wire so the stored files stay readable from the front end.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RiderSettings {
    pub name: String,
    pub weight_kg: f64,
    pub bike_kg: f64,
    pub ftp: f64,
    pub max_hr: f64,
}

impl Default for RiderSettings {
    fn default() -> Self {
        Self { name: "Rider".into(), weight_kg: 75.0, bike_kg: 9.0, ftp: 200.0, max_hr: 185.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PhysicsSettings {
    pub cda: f64,
    pub crr: f64,
    pub max_speed_kph: f64,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        Self { cda: 0.32, crr: 0.004, max_speed_kph: 80.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrainerMode {
    Sim,
    Erg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrainerSettings {
    /// Fraction (0–1) of the real gradient the trainer applies.
    pub difficulty: f64,
    pub mode: TrainerMode,
    pub erg_watts: f64,
    pub virtual_power: bool,
}

impl Default for TrainerSettings {
    fn default() -> Self {
        Self { difficulty: 0.75, mode: TrainerMode::Sim, erg_watts: 150.0, virtual_power: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BotSettings {
    pub enabled: bool,
    pub wkg: Vec<f64>,
}

impl Default for BotSettings {
    fn default() -> Self {
        Self { enabled: true, wkg: vec![1.5, 2.2, 3.0] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub rider: RiderSettings,
    pub physics: PhysicsSettings,
    pub trainer: TrainerSettings,
    pub units: String,
    pub bots: BotSettings,
    pub real_terrain: bool,
    /// Show buildings from OpenStreetMap along the route.
    pub buildings: bool,
    pub strava_auto_upload: bool,
    pub export_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rider: Default::default(),
            physics: Default::default(),
            trainer: Default::default(),
            units: "metric".into(),
            bots: Default::default(),
            real_terrain: true,
            buildings: true,
            strava_auto_upload: false,
            export_dir: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteMeta {
    pub id: String,
    pub name: String,
    pub distance: f64,
    pub ascent: f64,
    pub climbs: usize,
    pub added_at: u64,
    /// Simplified `[lat, lon]` outline for list previews.
    pub preview: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonalBest {
    /// Best complete time in seconds; 0 means the route has not been finished yet.
    pub time_s: f64,
    /// Distance (m) at each elapsed second of the best ride — replayed as the ghost.
    pub trace: Vec<f64>,
    /// Best time in seconds per detected climb index.
    pub climbs: BTreeMap<usize, f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub xp: u64,
    pub rides: u64,
    pub total_distance: f64,
    pub total_ascent: f64,
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RideSummary {
    pub id: String,
    pub route_id: String,
    pub route_name: String,
    pub started_at: u64,
    pub duration_s: f64,
    pub distance_m: f64,
    pub ascent_m: f64,
    pub avg_power: f64,
    pub max_power: f64,
    pub np: f64,
    pub avg_hr: f64,
    pub max_hr: f64,
    pub avg_cadence: f64,
    pub avg_speed: f64,
    pub kj: f64,
    pub tss: f64,
    pub completed: bool,
    pub is_pb: bool,
    pub xp: u64,
    pub new_badges: Vec<String>,
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strava_activity_id: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSummary {
    pub weight_kg: Option<f64>,
    pub weight_date: Option<String>,
    pub resting_hr: Option<u32>,
    pub max_hr_observed: Option<u32>,
    pub vo2max: Option<f64>,
    pub ftp: Option<u32>,
    pub date_of_birth: Option<String>,
    pub cycling_workouts: u32,
    pub records: u64,
}
