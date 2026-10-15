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
    /// Steepest gradient (fraction) sent to the trainer, after scaling; 0 means no limit.
    pub max_grade: f64,
    /// Fraction of the (already scaled) downhill gradient applied, so descents don't spin out.
    pub downhill: f64,
    /// Hold workout targets in ERG mode (otherwise targets are shown and the gradient applies).
    pub workout_erg: bool,
}

impl Default for TrainerSettings {
    fn default() -> Self {
        Self { difficulty: 0.75, mode: TrainerMode::Sim, erg_watts: 150.0, virtual_power: false, max_grade: 0.15, downhill: 0.5, workout_erg: true }
    }
}

impl TrainerSettings {
    /// The gradient to send the trainer for a road gradient.
    pub fn trainer_grade(&self, grade: f64) -> f64 {
        let mut g = grade * self.difficulty;
        if g < 0.0 {
            g *= self.downhill;
        }
        if self.max_grade > 0.0 {
            g = g.clamp(-self.max_grade, self.max_grade);
        }
        g
    }
}

/// A season target, e.g. 1000 km before spring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Goal {
    pub name: String,
    pub distance_km: f64,
    /// `YYYY-MM-DD`; empty means the next 20 March.
    pub deadline: String,
    /// Rides on or after this date count; empty means the goal's first day is when it was set.
    pub from: String,
}

impl Default for Goal {
    fn default() -> Self {
        Self { name: "Winter miles".into(), distance_km: 1000.0, deadline: String::new(), from: String::new() }
    }
}

/// How the world looks: time of day and weather.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorldLook {
    /// "dawn", "day", "evening", "dusk", "night" or "clock" (the real time of day).
    pub time_of_day: String,
    /// "clear", "cloudy", "rain", "fog", "snow" or "live" (the weather at the route now).
    pub weather: String,
    /// Place names, summits and points of interest along the route.
    pub landmarks: bool,
    /// Street-level photos from Mapillary (needs a free client token).
    pub photos: bool,
    pub mapillary_token: String,
}

impl Default for WorldLook {
    fn default() -> Self {
        Self { time_of_day: "day".into(), weather: "clear".into(), landmarks: true, photos: false, mapillary_token: String::new() }
    }
}

/// A fan and lights driven by the ride, through a smart plug or Home Assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SmartHome {
    /// "none", "homeassistant", "shelly", "tasmota" or "webhook".
    pub kind: String,
    /// Home Assistant base URL, the plug's address, or the webhook URL (with `{level}`, `{power}`, `{zone}`).
    pub url: String,
    /// Home Assistant fan entity (e.g. `fan.desk_fan`) or switch entity.
    pub fan_entity: String,
    /// Home Assistant light entity, coloured by power zone.
    pub light_entity: String,
    /// What drives the fan: "speed", "power" or "hr".
    pub fan_by: String,
    /// Fan off below this speed (km/h), power (W) or heart rate.
    pub fan_from: f64,
    /// Fan full at or above this.
    pub fan_full: f64,
}

impl Default for SmartHome {
    fn default() -> Self {
        Self { kind: "none".into(), url: String::new(), fan_entity: String::new(), light_entity: String::new(), fan_by: "speed".into(), fan_from: 10.0, fan_full: 35.0 }
    }
}

/// Group rides through a relay server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupSettings {
    pub relay_url: String,
}

impl Default for GroupSettings {
    fn default() -> Self {
        Self { relay_url: "ws://127.0.0.1:47800".into() }
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
    /// Show forests, water, bridges and tunnels from OpenStreetMap.
    pub scenery: bool,
    pub strava_auto_upload: bool,
    pub export_dir: String,
    pub goal: Goal,
    pub look: WorldLook,
    pub smart: SmartHome,
    pub group: GroupSettings,
    /// Draw the 3D world with the GPU directly instead of in the web page (macOS, experimental).
    pub native_renderer: bool,
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
            scenery: true,
            strava_auto_upload: false,
            export_dir: String::new(),
            goal: Default::default(),
            look: Default::default(),
            smart: Default::default(),
            group: Default::default(),
            native_renderer: false,
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
    /// Best efforts on segments (climbs and Strava segments), by segment id.
    pub segments: BTreeMap<String, crate::segments::SegmentBest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub xp: u64,
    pub rides: u64,
    pub total_distance: f64,
    pub total_ascent: f64,
    pub badges: Vec<String>,
    pub segment_prs: u64,
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
    /// Best average power for each of `fitness::CURVE_DURATIONS`.
    #[serde(default)]
    pub curve: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workout: Option<String>,
    /// FTP this ride suggests, when it is higher than the one set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ftp_estimate: Option<crate::fitness::FtpEstimate>,
    /// Segment bests set on this ride.
    #[serde(default)]
    pub segment_prs: Vec<String>,
    #[serde(default)]
    pub group: bool,
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
