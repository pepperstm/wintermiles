//! Segments (timed stretches of the route, from the route's own climbs or from
//! Strava) and putting other GPS tracks onto the route, for segments and ghosts.

use serde::{Deserialize, Serialize};

use crate::gpx::Route;

/// Personal-best splits are kept every this many metres along a segment.
pub const SPLIT_M: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentSource {
    Climb,
    Strava,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: String,
    pub name: String,
    pub source: SegmentSource,
    /// Start and end, in metres along the route.
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub avg_grade: f64,
    /// Fastest time on Strava ("KOM"/"QOM"), as Strava formats it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

/// Best time on a segment, with the elapsed time at every [`SPLIT_M`] metres so it can be raced live.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SegmentBest {
    pub time_s: f64,
    pub splits: Vec<f64>,
}

impl SegmentBest {
    /// Elapsed time of the best effort `offset` metres into the segment.
    pub fn at(&self, offset: f64) -> Option<f64> {
        if self.splits.len() < 2 {
            return None;
        }
        let f = (offset / SPLIT_M).max(0.0);
        let i = (f.floor() as usize).min(self.splits.len() - 2);
        let t = (f - i as f64).min(1.0);
        Some(self.splits[i] + (self.splits[i + 1] - self.splits[i]) * t)
    }
}

/// Decodes a Google-encoded polyline (as Strava sends segment shapes) into `(lat, lon)` pairs.
pub fn decode_polyline(s: &str) -> Vec<(f64, f64)> {
    let bytes = s.as_bytes();
    let (mut i, mut lat, mut lon) = (0usize, 0i64, 0i64);
    let mut out = Vec::new();
    let next = |i: &mut usize| -> Option<i64> {
        let (mut result, mut shift) = (0i64, 0);
        loop {
            let b = (*bytes.get(*i)? as i64) - 63;
            *i += 1;
            result |= (b & 0x1f) << shift;
            shift += 5;
            if b < 0x20 {
                break;
            }
        }
        Some(if result & 1 != 0 { !(result >> 1) } else { result >> 1 })
    };
    while i < bytes.len() {
        let (Some(dlat), Some(dlon)) = (next(&mut i), next(&mut i)) else { break };
        lat += dlat;
        lon += dlon;
        out.push((lat as f64 * 1e-5, lon as f64 * 1e-5));
    }
    out
}

/// Distance along the route of the point nearest to `(x, z)`, searching samples `from..to`,
/// with how far off the route it is.
fn nearest(route: &Route, x: f64, z: f64, from: usize, to: usize) -> Option<(f64, f64)> {
    let to = to.min(route.n.saturating_sub(1));
    let mut best: Option<(f64, f64)> = None;
    for i in from..to {
        let (ax, az, bx, bz) = (route.x[i], route.z[i], route.x[i + 1], route.z[i + 1]);
        let (dx, dz) = (bx - ax, bz - az);
        let len2 = dx * dx + dz * dz;
        let t = if len2 > 0.0 { (((x - ax) * dx + (z - az) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let off = (x - ax - dx * t).hypot(z - az - dz * t);
        if best.is_none_or(|b| off < b.1) {
            best = Some(((i as f64 + t) * route.step, off));
        }
    }
    best
}

/// Follows a GPS track along the route. Each point gets the distance along the route it
/// corresponds to, or `None` where the track is more than `tolerance` metres off it.
/// Distances never go backwards, so a route that passes the same place twice is followed in order.
pub fn follow_track(route: &Route, points: &[(f64, f64)], tolerance: f64) -> Vec<Option<f64>> {
    let mut at = 0usize;
    let mut found_once = false;
    points
        .iter()
        .map(|&(lat, lon)| {
            let (x, z) = route.to_local(lat, lon);
            // Before the track first meets the route, look along all of it; after, only a little way ahead.
            let (from, to) = if found_once { (at, at + (400.0 / route.step) as usize + 2) } else { (0, route.n) };
            let hit = nearest(route, x, z, from, to).filter(|h| h.1 <= tolerance)?;
            found_once = true;
            at = ((hit.0 / route.step) as usize).saturating_sub(1);
            Some(hit.0)
        })
        .collect()
}

/// Places a Strava segment's shape on the route: `(start, end)` in metres, if the route rides all of it.
pub fn place_segment(route: &Route, shape: &[(f64, f64)], length: f64) -> Option<(f64, f64)> {
    let (first, last) = (shape.first()?, shape.last()?);
    let (fx, fz) = route.to_local(first.0, first.1);
    // Try every place the route passes the segment's start, in order.
    let mut i = 0;
    while i + 1 < route.n {
        let near_start = (route.x[i] - fx).hypot(route.z[i] - fz) < 30.0;
        if !near_start {
            i += 1;
            continue;
        }
        let (start, off) = nearest(route, fx, fz, i, i + (60.0 / route.step) as usize + 2)?;
        if off > 25.0 {
            i += 1;
            continue;
        }
        // Walk the shape forward along the route from there.
        let mut at = (start / route.step) as usize;
        let mut ok = true;
        let mut end = start;
        for &(lat, lon) in shape.iter().skip(1) {
            let (x, z) = route.to_local(lat, lon);
            match nearest(route, x, z, at, at + (300.0 / route.step) as usize + 2) {
                Some((d, off)) if off <= 30.0 => {
                    at = ((d / route.step) as usize).max(at);
                    end = end.max(d);
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        let (lx, lz) = route.to_local(last.0, last.1);
        let close_at_end = nearest(route, lx, lz, (end / route.step) as usize, (end / route.step) as usize + 3).is_some_and(|h| h.1 <= 25.0);
        if ok && close_at_end && end - start > 0.0 && ((end - start) - length).abs() <= length * 0.15 + 20.0 {
            return Some((start, end));
        }
        // Skip past this pass of the start before trying the next.
        i += (60.0 / route.step) as usize + 1;
    }
    None
}

/// Turns a timed track (Unix seconds, lat, lon) into a ghost: the distance along the route at
/// each whole second, starting when the track leaves the route's start. Stops where the track
/// leaves the route for good. `None` if it never rode the start of the route.
pub fn ghost_trace(route: &Route, track: &[(f64, f64, f64)]) -> Option<Vec<f64>> {
    let followed = follow_track(route, &track.iter().map(|p| (p.1, p.2)).collect::<Vec<_>>(), 40.0);
    let first = followed.iter().position(|d| d.is_some_and(|d| d < 150.0))?;
    // Begin at the last point still at the very start, so a long wait before setting off doesn't count.
    let mut begin = first;
    for (j, d) in followed.iter().enumerate().skip(first) {
        match d {
            Some(d) if *d < 5.0 => begin = j,
            Some(d) if *d > 150.0 => break,
            _ => {}
        }
    }
    let t0 = track[begin].0;
    let mut timed: Vec<(f64, f64)> = vec![(0.0, followed[begin].unwrap_or(0.0))];
    let mut missing = 0.0;
    for j in begin + 1..track.len() {
        match followed[j] {
            Some(d) => {
                missing = 0.0;
                let last = timed.last().unwrap().1;
                timed.push((track[j].0 - t0, d.max(last)));
            }
            None => {
                missing += 1.0;
                if missing > 30.0 {
                    break;
                }
            }
        }
    }
    if timed.len() < 10 {
        return None;
    }
    let end_t = timed.last().unwrap().0.ceil() as usize;
    let mut out = Vec::with_capacity(end_t + 1);
    let mut k = 0;
    for s in 0..=end_t {
        let t = s as f64;
        while k + 1 < timed.len() && timed[k + 1].0 < t {
            k += 1;
        }
        let (a, b) = (timed[k], timed[(k + 1).min(timed.len() - 1)]);
        let d = if b.0 > a.0 { a.1 + (b.1 - a.1) * ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) } else { a.1 };
        out.push((d * 10.0).round() / 10.0);
    }
    Some(out)
}
