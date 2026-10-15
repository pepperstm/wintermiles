//! XP, levels and badges.

use serde::Serialize;

use crate::types::Profile;

#[derive(Debug, Clone, Default)]
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
    /// A structured workout was ridden to the end.
    pub workout_done: bool,
    pub ramp_test: bool,
    /// Finished ahead of a ghost imported from someone else's ride.
    pub beat_other_ghost: bool,
    pub segment_prs: usize,
    pub group: bool,
    /// Started between 21:00 and 05:00 local time.
    pub night: bool,
    /// Consecutive weeks (including this one) with at least one ride.
    pub week_streak: u32,
    /// Different routes ridden, including this one.
    pub routes_ridden: usize,
    /// The season goal was reached by this ride.
    pub goal_reached: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Badge {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub const BADGES: [Badge; 23] = [
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
    Badge { id: "five-hundred", name: "Halfway Home", description: "Ride 500 km in total" },
    Badge { id: "workout", name: "Structured", description: "Finish a structured workout" },
    Badge { id: "ramp", name: "Lab Rat", description: "Complete an FTP ramp test" },
    Badge { id: "ghost-hunter", name: "Ghost Hunter", description: "Finish ahead of someone else's ghost" },
    Badge { id: "segment", name: "Segment Hunter", description: "Set a best time on a Strava segment" },
    Badge { id: "segments-10", name: "Leaderboard Regular", description: "Set ten segment bests" },
    Badge { id: "group", name: "Peloton", description: "Ride with friends in a group ride" },
    Badge { id: "night", name: "Night Owl", description: "Start a ride after 9 pm" },
    Badge { id: "streak-4", name: "Consistent", description: "Ride every week for four weeks" },
    Badge { id: "streak-12", name: "Iron Winter", description: "Ride every week for twelve weeks" },
    Badge { id: "explorer", name: "Explorer", description: "Ride ten different routes" },
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
        "five-hundred" => p.total_distance >= 500_000.0,
        "workout" => r.workout_done,
        "ramp" => r.ramp_test && r.workout_done,
        "ghost-hunter" => r.beat_other_ghost,
        "segment" => r.segment_prs > 0,
        "segments-10" => p.segment_prs >= 10,
        "group" => r.group,
        "night" => r.night,
        "streak-4" => r.week_streak >= 4,
        "streak-12" => r.week_streak >= 12,
        "explorer" => r.routes_ridden >= 10,
        _ => false,
    }
}

pub fn xp_for_ride(r: &RideFacts) -> u64 {
    let bonus = if r.completed { 50.0 } else { 0.0 }
        + if r.beat_pb { 100.0 } else { 0.0 }
        + if r.workout_done { 75.0 } else { 0.0 }
        + if r.group { 50.0 } else { 0.0 }
        + 25.0 * r.segment_prs.min(10) as f64
        + if r.goal_reached { 500.0 } else { 0.0 };
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
    profile.segment_prs += ride.segment_prs as u64;
    let new_badges: Vec<String> = BADGES
        .iter()
        .filter(|b| !profile.badges.iter().any(|id| id == b.id) && earned(b.id, ride, profile))
        .map(|b| b.id.to_string())
        .collect();
    profile.badges.extend(new_badges.iter().cloned());
    (xp, new_badges)
}

/// Week number (Monday-based) of a day counted from the Unix epoch.
fn week_of(day: i64) -> i64 {
    // 1 January 1970 was a Thursday.
    (day + 3).div_euclid(7)
}

/// Ride streaks from the days (since the epoch, local time) that had a ride.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Streaks {
    /// Consecutive weeks with a ride, ending this week (or last week, if none yet this week).
    pub weeks: u32,
    pub best_weeks: u32,
    /// Consecutive days with a ride, ending today or yesterday.
    pub days: u32,
}

pub fn streaks(ride_days: &[i64], today: i64) -> Streaks {
    let mut days: Vec<i64> = ride_days.to_vec();
    days.sort_unstable();
    days.dedup();
    let mut weeks: Vec<i64> = days.iter().map(|d| week_of(*d)).collect();
    weeks.dedup();
    let run_ending = |set: &[i64], end: i64| -> u32 {
        let mut n = 0;
        while set.binary_search(&(end - n as i64)).is_ok() {
            n += 1;
        }
        n
    };
    let this_week = week_of(today);
    let w = run_ending(&weeks, this_week).max(run_ending(&weeks, this_week - 1));
    let d = run_ending(&days, today).max(run_ending(&days, today - 1));
    let mut best = 0;
    let mut run = 0;
    for (i, wk) in weeks.iter().enumerate() {
        run = if i > 0 && weeks[i - 1] == wk - 1 { run + 1 } else { 1 };
        best = best.max(run);
    }
    Streaks { weeks: w, best_weeks: best, days: d }
}
