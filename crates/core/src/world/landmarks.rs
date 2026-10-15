//! Signs along the route: place names where the road enters a town or village,
//! names for the summits of its climbs, and points of interest by the roadside.

use serde::Serialize;

use super::mvt::Label;
use super::Scenery;
use crate::gpx::Route;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Landmark {
    /// Metres along the route where the sign stands.
    pub distance: f64,
    /// +1 to the right of the road, −1 to the left.
    pub side: f64,
    /// A place kind ("town", "village", …), "summit", or a point-of-interest kind ("cafe", …).
    pub kind: String,
    pub name: String,
    /// Extra text, e.g. a summit's height.
    pub detail: String,
}

/// Nearest point of the route to `(x, z)`: distance along it, distance off it, and which side.
fn nearest(route: &Route, x: f64, z: f64) -> (f64, f64, f64) {
    let mut best = (0.0, f64::INFINITY, 1.0);
    for i in 0..route.n.saturating_sub(1) {
        let (ax, az, bx, bz) = (route.x[i], route.z[i], route.x[i + 1], route.z[i + 1]);
        let (dx, dz) = (bx - ax, bz - az);
        let len2 = dx * dx + dz * dz;
        let t = if len2 > 0.0 { (((x - ax) * dx + (z - az) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let off = (x - ax - dx * t).hypot(z - az - dz * t);
        if off < best.1 {
            // Positive sides are along (dz, −dx), as the scene's `sideOfRoad` places them.
            let cross = dx * (z - az) - dz * (x - ax);
            best = ((i as f64 + t) * route.step, off, if cross >= 0.0 { -1.0 } else { 1.0 });
        }
    }
    best
}

/// How close the route must pass a place for it to get a name sign.
fn place_reach(kind: &str) -> Option<f64> {
    Some(match kind {
        "city" => 3000.0,
        "town" => 1500.0,
        "village" | "suburb" => 600.0,
        "hamlet" => 250.0,
        "locality" => 150.0,
        _ => return None,
    })
}

pub fn landmarks(route: &Route, scenery: Option<&Scenery>) -> Vec<Landmark> {
    let labels: &[Label] = scenery.map_or(&[], |s| s.labels.as_slice());
    let placed: Vec<(&Label, f64, f64, f64)> = labels
        .iter()
        .map(|l| {
            let (x, z) = route.to_local(l.at[0], l.at[1]);
            let (d, off, side) = nearest(route, x, z);
            (l, d, off, side)
        })
        .collect();
    let mut out: Vec<Landmark> = Vec::new();

    // Name signs, a little before the closest approach to the place's centre.
    for &(l, d, off, side) in &placed {
        let Some(reach) = place_reach(&l.kind) else { continue };
        if off > reach || l.name.is_empty() {
            continue;
        }
        let at = (d - (reach * 0.35).min(350.0)).max(20.0);
        if out.iter().any(|m| m.name == l.name && (m.distance - at).abs() < 3000.0) {
            continue;
        }
        out.push(Landmark { distance: at, side, kind: l.kind.clone(), name: l.name.clone(), detail: String::new() });
    }

    // Summits: named after a place or point of interest right at the top, if there is one.
    for (i, c) in route.climbs.iter().enumerate() {
        let top = (c.end / route.step).round() as usize;
        let ele = route.ele[top.min(route.n - 1)];
        let name = placed
            .iter()
            .filter(|(l, d, off, _)| !l.name.is_empty() && *off < 400.0 && (d - c.end).abs() < 500.0 && l.kind != "water")
            .min_by(|a, b| (a.1 - c.end).abs().total_cmp(&(b.1 - c.end).abs()))
            .map_or_else(|| format!("Climb {}", i + 1), |(l, ..)| l.name.clone());
        out.push(Landmark { distance: c.end, side: -1.0, kind: "summit".into(), name, detail: format!("{} m", ele.round()) });
    }

    // Points of interest right by the road; one of each name and kind per stretch.
    let mut pois = 0;
    for &(l, d, off, side) in &placed {
        if place_reach(&l.kind).is_some() || off > 45.0 || pois >= 300 {
            continue;
        }
        if out.iter().any(|m| m.kind == l.kind && m.name == l.name && (m.distance - d).abs() < 600.0) {
            continue;
        }
        let name = if l.name.is_empty() { "Drinking water".to_string() } else { l.name.clone() };
        out.push(Landmark { distance: d, side, kind: l.kind.clone(), name, detail: String::new() });
        pois += 1;
    }
    out.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    out
}
