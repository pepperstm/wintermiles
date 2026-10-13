//! XP, levels and badges.

use serde::Serialize;

use crate::types::Profile;

#[derive(Debug, Clone)]
pub struct RideFacts {
    pub distance_m: f64,
    pub ascent_m: f64,
    pub duration_s: f64,
    pub completed: bool,
    /// True only when an existing best time was beaten.
    pub beat_pb: bool,
    pub np: f64,
    pub ftp: f64,
    /// Categories of the climbs finished during this ride.
    pub climb_categories: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Badge {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub const BADGES: [Badge; 12] = [
    Badge { id: "first-ride", name: "Rolling", description: "Ride your first kilometre" },
    Badge { id: "finisher", name: "Finisher", description: "Complete a whole route" },
    Badge { id: "hour", name: "Hour of Power", description: "Ride for 60 minutes in one go" },
    Badge { id: "climber", name: "Climber", description: "Climb 500 m in one ride" },
    Badge { id: "goat", name: "Mountain Goat", description: "Climb 1500 m in one ride" },
    Badge { id: "big-climb", name: "Col Collector", description: "Summit a Cat 1 or HC climb" },
    Badge { id: "pb", name: "Ghostbuster", description: "Beat your own best time on a route" },
    Badge { id: "threshold", name: "On the Rivet", description: "Hold 90% of FTP (normalised) for 20+ minutes" },
    Badge { id: "five", name: "Habit Forming", description: "Finish five rides" },
    Badge { id: "century", name: "Century", description: "Ride 100 km in total" },
    Badge { id: "thousand", name: "Winter Base", description: "Ride 1000 km in total" },
    Badge { id: "everest", name: "Everest", description: "Climb 8848 m in total" },
];

fn earned(id: &str, r: &RideFacts, p: &Profile) -> bool {
    match id {
        "first-ride" => r.distance_m >= 1000.0,
        "finisher" => r.completed,
        "hour" => r.duration_s >= 3600.0,
        "climber" => r.ascent_m >= 500.0,
        "goat" => r.ascent_m >= 1500.0,
        "big-climb" => r.climb_categories.iter().any(|c| c == "1" || c == "HC"),
        "pb" => r.beat_pb,
        "threshold" => r.duration_s >= 1200.0 && r.ftp > 0.0 && r.np >= 0.9 * r.ftp,
        "five" => p.rides >= 5,
        "century" => p.total_distance >= 100_000.0,
        "thousand" => p.total_distance >= 1_000_000.0,
        "everest" => p.total_ascent >= 8848.0,
        _ => false,
    }
}

pub fn xp_for_ride(r: &RideFacts) -> u64 {
    let bonus = if r.completed { 50.0 } else { 0.0 } + if r.beat_pb { 100.0 } else { 0.0 };
    (r.distance_m / 1000.0 * 10.0 + r.ascent_m / 5.0 + bonus).round() as u64
}

/// Returns `(level, xp into this level, xp spanned by this level)`.
pub fn level_for(xp: u64) -> (u64, u64, u64) {
    let level = (xp as f64 / 200.0).sqrt().floor() as u64 + 1;
    let floor = 200 * (level - 1).pow(2);
    let next = 200 * level.pow(2);
    (level, xp - floor, next - floor)
}

/// Folds a finished ride into the profile; returns the XP gained and the newly earned badge ids.
pub fn apply_ride(profile: &mut Profile, ride: &RideFacts) -> (u64, Vec<String>) {
    let xp = xp_for_ride(ride);
    profile.xp += xp;
    profile.rides += 1;
    profile.total_distance += ride.distance_m;
    profile.total_ascent += ride.ascent_m;
    let new_badges: Vec<String> = BADGES
        .iter()
        .filter(|b| !profile.badges.iter().any(|id| id == b.id) && earned(b.id, ride, profile))
        .map(|b| b.id.to_string())
        .collect();
    profile.badges.extend(new_badges.iter().cloned());
    (xp, new_badges)
}
