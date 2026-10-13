//! Buildings from OpenStreetMap: parsing footprints out of an Overpass API
//! reply and extruding them into simple blocks standing on the terrain.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::height::{hash2, HeightField};
use super::CHUNK;
use crate::gpx::Route;

/// A building outline as mapped: a closed ring of `[lat, lon]` and a height in metres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub id: u64,
    pub height: f32,
    pub ring: Vec<[f64; 2]>,
}

#[derive(Deserialize)]
struct Reply {
    elements: Vec<Element>,
}

#[derive(Deserialize)]
struct Element {
    id: u64,
    #[serde(default)]
    tags: HashMap<String, String>,
    /// Present on ways. Entries can be null where a node fell outside the query.
    #[serde(default)]
    geometry: Vec<Option<LatLon>>,
    /// Present on multipolygon relations.
    #[serde(default)]
    members: Vec<Member>,
}

#[derive(Deserialize)]
struct Member {
    #[serde(default)]
    role: String,
    #[serde(default)]
    geometry: Vec<Option<LatLon>>,
}

#[derive(Deserialize, Clone, Copy)]
struct LatLon {
    lat: f64,
    lon: f64,
}

/// Reads the number at the start of a tag value such as `"12"`, `"7.5 m"` or `"3;4"`.
fn leading_number(s: &str) -> Option<f32> {
    let s = s.trim();
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    s[..end].parse().ok()
}

/// Height in metres from the mapped tags, falling back to a typical height for the building type.
fn height_from_tags(tags: &HashMap<String, String>) -> f32 {
    if let Some(h) = tags.get("height").and_then(|v| leading_number(v)) {
        return h.clamp(2.5, 150.0);
    }
    if let Some(levels) = tags.get("building:levels").and_then(|v| leading_number(v)) {
        return (levels * 3.0 + 1.0).clamp(2.5, 150.0);
    }
    match tags.get("building").map(String::as_str).unwrap_or("yes") {
        "garage" | "garages" | "shed" | "hut" | "cabin" | "carport" | "kiosk" | "greenhouse" | "static_caravan" => 3.0,
        "house" | "detached" | "semidetached_house" | "bungalow" | "farm" | "terrace" | "chalet" => 6.5,
        "residential" | "barn" | "farm_auxiliary" | "retail" | "supermarket" | "industrial" | "warehouse" | "school" => 8.0,
        "apartments" | "hotel" | "office" | "commercial" | "hospital" | "university" | "public" | "civic" => 13.0,
        "church" | "cathedral" | "chapel" | "mosque" | "temple" => 14.0,
        _ => 7.0,
    }
}

fn closed_ring(geometry: &[Option<LatLon>]) -> Option<Vec<[f64; 2]>> {
    let pts: Vec<LatLon> = geometry.iter().flatten().copied().collect();
    let (first, last) = (pts.first()?, pts.last()?);
    // An outline must come back to where it started; anything else is a fragment.
    (pts.len() >= 4 && pts.len() == geometry.len() && first.lat == last.lat && first.lon == last.lon).then(|| pts.iter().map(|p| [p.lat, p.lon]).collect())
}

/// Extracts building footprints from an Overpass API reply produced with `out geom`.
pub fn parse_overpass(json: &str) -> Result<Vec<Footprint>, String> {
    let reply: Reply = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from the OpenStreetMap server: {e}"))?;
    let mut out = Vec::new();
    for e in reply.elements {
        let underground = e.tags.get("location").is_some_and(|v| v == "underground") || e.tags.get("layer").is_some_and(|v| v.trim_start().starts_with('-'));
        // "roof" is a canopy with no walls (petrol station forecourts, shelters).
        if underground || e.tags.get("building").is_some_and(|v| v == "roof" || v == "no") {
            continue;
        }
        let height = height_from_tags(&e.tags);
        if let Some(ring) = closed_ring(&e.geometry) {
            out.push(Footprint { id: e.id, height, ring });
        }
        // Multipolygons: each closed outer ring becomes a block; courtyards are ignored.
        for (k, m) in e.members.iter().enumerate().filter(|(_, m)| m.role == "outer") {
            if let Some(ring) = closed_ring(&m.geometry) {
                out.push(Footprint { id: e.id.wrapping_mul(31).wrapping_add(k as u64), height, ring });
            }
        }
    }
    Ok(out)
}

/// The Overpass QL query for every building within `radius_m` of a stretch of the route.
pub fn overpass_query(route: &Route, from: usize, to: usize, radius_m: f64, spacing_m: f64) -> String {
    let stride = ((spacing_m / route.step).round() as usize).max(1);
    let mut line: Vec<String> = (from..to).step_by(stride).map(|i| format!("{:.5},{:.5}", route.lat[i], route.lon[i])).collect();
    line.push(format!("{:.5},{:.5}", route.lat[to - 1], route.lon[to - 1]));
    let line = line.join(",");
    format!("[out:json][timeout:25];(way[\"building\"](around:{radius_m:.0},{line});relation[\"building\"](around:{radius_m:.0},{line}););out geom;")
}

struct Building {
    /// Outline in local metres (x, z), without a repeated closing point, wound so the area is positive.
    ring: Vec<(f64, f64)>,
    /// Terrain height under each outline vertex.
    ground: Vec<f64>,
    height: f64,
    centre: (f64, f64),
    radius: f64,
    seed: i32,
}

fn signed_area(ring: &[(f64, f64)]) -> f64 {
    let n = ring.len();
    (0..n).map(|i| ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1).sum::<f64>() / 2.0
}

fn cross(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0)
}

fn in_triangle(p: (f64, f64), a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> bool {
    cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
}

/// Ear-clipping triangulation of a simple polygon with positive signed area.
pub(super) fn triangulate(ring: &[(f64, f64)]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..ring.len()).collect();
    let mut out = Vec::with_capacity(ring.len().saturating_sub(2));
    while idx.len() > 3 {
        let m = idx.len();
        let ear = (0..m).find(|&k| {
            let (a, b, c) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            // A convex corner with no other vertex inside its triangle can be cut off.
            cross(ring[a], ring[b], ring[c]) > 1e-9 && !idx.iter().any(|&j| j != a && j != b && j != c && in_triangle(ring[j], ring[a], ring[b], ring[c]))
        });
        // Self-intersecting or degenerate outlines can run out of ears; close what is left as a fan.
        let Some(k) = ear else { break };
        out.push([idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]]);
        idx.remove(k);
    }
    for k in 1..idx.len().saturating_sub(1) {
        out.push([idx[0], idx[k], idx[k + 1]]);
    }
    out
}

fn inside(ring: &[(f64, f64)], x: f64, z: f64) -> bool {
    let n = ring.len();
    let mut hit = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > z) != (b.1 > z) && x < (b.0 - a.0) * (z - a.1) / (b.1 - a.1) + a.0 {
            hit = !hit;
        }
    }
    hit
}

// Wall and roof colours, authored in sRGB.
const WALLS: [[f64; 3]; 6] = [[0.86, 0.82, 0.74], [0.80, 0.74, 0.66], [0.90, 0.88, 0.84], [0.74, 0.70, 0.66], [0.84, 0.76, 0.62], [0.70, 0.62, 0.56]];
const ROOFS: [[f64; 3]; 4] = [[0.55, 0.27, 0.20], [0.36, 0.36, 0.40], [0.45, 0.33, 0.28], [0.50, 0.50, 0.50]];
/// Storey height, and the rough width of one window bay, in metres.
const STOREY: f64 = 3.0;
const BAY: f64 = 3.2;
/// A spot on the window tile that is plain wall, for roofs and gables.
const PLAIN: [f32; 2] = [0.04, 0.04];
/// Nothing is built this close to the road centreline: a mapped outline there means
/// the GPS track and the map disagree, and a wall across the road helps no one.
const ROAD_CLEARANCE: f64 = 5.5;

/// Triangle soup for a chunk's buildings (three vertices per triangle).
#[derive(Default)]
pub struct BuildingMesh {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub colors: Vec<f32>,
    /// Texture coordinates into a repeating one-window tile: on walls, one unit
    /// per window bay across and per storey down from the eaves.
    pub uvs: Vec<f32>,
}

#[derive(Default)]
pub struct Buildings {
    by_chunk: HashMap<(i32, i32), Vec<Building>>,
    count: usize,
}

fn chunk_of(x: f64, z: f64) -> (i32, i32) {
    ((x / CHUNK).floor() as i32, (z / CHUNK).floor() as i32)
}

impl Buildings {
    /// Places footprints in the route's local frame, dropping any that are
    /// implausible or that would stand on the road.
    pub fn new(route: &Route, field: &HeightField, footprints: &[Footprint]) -> Self {
        let mut out = Buildings::default();
        for f in footprints {
            let mut ring: Vec<(f64, f64)> = f.ring.iter().map(|p| route.to_local(p[0], p[1])).collect();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            ring.dedup();
            // Mappers often add extra points along a straight wall; without them a plain
            // rectangle is recognisable as one (and gets a pitched roof).
            let mut k = 0;
            while ring.len() > 3 && k < ring.len() {
                let m = ring.len();
                let (a, p, c) = (ring[(k + m - 1) % m], ring[k], ring[(k + 1) % m]);
                let off_line = ((c.0 - a.0) * (p.1 - a.1) - (c.1 - a.1) * (p.0 - a.0)).abs() / (c.0 - a.0).hypot(c.1 - a.1).max(1e-9);
                if off_line < 0.25 {
                    ring.remove(k);
                } else {
                    k += 1;
                }
            }
            if ring.len() < 3 {
                continue;
            }
            let area = signed_area(&ring);
            if !(8.0..=60_000.0).contains(&area.abs()) {
                continue;
            }
            if area < 0.0 {
                ring.reverse();
            }
            let n = ring.len();
            let centre = (ring.iter().map(|p| p.0).sum::<f64>() / n as f64, ring.iter().map(|p| p.1).sum::<f64>() / n as f64);
            let ground: Vec<(f64, f64)> = ring.iter().map(|p| field.height(p.0, p.1)).collect();
            // Corners, wall midpoints and the middle must all be clear of the road.
            let mid_clear = (0..n).all(|i| {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                field.height((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0).1 >= ROAD_CLEARANCE
            });
            if ground.iter().any(|g| g.1 < ROAD_CLEARANCE) || !mid_clear || field.height(centre.0, centre.1).1 < ROAD_CLEARANCE {
                continue;
            }
            let radius = ring.iter().map(|p| (p.0 - centre.0).hypot(p.1 - centre.1)).fold(0.0, f64::max);
            let building = Building { ground: ground.iter().map(|g| g.0).collect(), ring, height: f.height as f64, centre, radius, seed: (f.id ^ (f.id >> 32)) as i32 };
            out.by_chunk.entry(chunk_of(centre.0, centre.1)).or_default().push(building);
            out.count += 1;
        }
        out
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// True if the point is inside a building's outline (used to keep trees out of houses).
    pub fn covers(&self, x: f64, z: f64) -> bool {
        let (cx, cz) = chunk_of(x, z);
        // A building belongs to the chunk holding its centre but may reach into a neighbour.
        (cx - 1..=cx + 1).flat_map(|ix| (cz - 1..=cz + 1).map(move |iz| (ix, iz))).filter_map(|key| self.by_chunk.get(&key)).flatten().any(|b| {
            (x - b.centre.0).hypot(z - b.centre.1) <= b.radius + 1.5 && inside(&b.ring, x, z)
        })
    }

    /// Extrudes the buildings whose centre lies in chunk `(cx, cz)`.
    pub fn mesh(&self, cx: i32, cz: i32) -> BuildingMesh {
        let mut mesh = BuildingMesh::default();
        let Some(list) = self.by_chunk.get(&(cx, cz)) else { return mesh };
        for b in list {
            let n = b.ring.len();
            let lowest = b.ground.iter().cloned().fold(f64::INFINITY, f64::min);
            let highest = b.ground.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let mean = b.ground.iter().sum::<f64>() / n as f64;
            // On a slope the walls reach down past the lowest corner. The eaves sit a
            // building's height above the average ground, so the uphill side is dug in
            // (as real hillside buildings are) but always shows at least a low wall.
            let (base, top) = (lowest - 1.5, (mean + b.height).max(highest + 2.5));
            let linear = |c: [f64; 3], k: f64| c.map(|v| ((v * k).clamp(0.0, 1.0)).powf(2.2) as f32);
            // A slight per-building brightness shift keeps a terrace from reading as one slab.
            let shade = 0.92 + hash2(b.seed, 7) * 0.16;
            let wall = WALLS[(hash2(b.seed, 1) * WALLS.len() as f64) as usize % WALLS.len()];
            let area = signed_area(&b.ring);
            let roof = if area > 600.0 { ROOFS[3] } else { ROOFS[(hash2(b.seed, 2) * 3.0) as usize % 3] };
            // Darker at the foot, lighter at the eaves: cheap depth without textures.
            let (wall_top, wall_base, roof) = (linear(wall, shade), linear(wall, shade * 0.8), linear(roof, shade));

            let at = |p: (f64, f64), y: f64| [p.0, y, p.1];
            for i in 0..n {
                let (a, c) = (b.ring[i], b.ring[(i + 1) % n]);
                // The ring has positive area in (x, z), which puts "outside" on this side of each edge.
                let out = [c.1 - a.1, 0.0, a.0 - c.0];
                // A whole number of window bays per wall, so no window is cut by a corner.
                let bays = ((c.0 - a.0).hypot(c.1 - a.1) / BAY).round().max(1.0) as f32;
                let down = ((top - base) / STOREY) as f32;
                mesh.triangle([at(a, base), at(c, base), at(c, top)], out, [wall_base, wall_base, wall_top], [[0.0, down], [bays, down], [bays, 0.0]]);
                mesh.triangle([at(a, base), at(c, top), at(a, top)], out, [wall_base, wall_top, wall_top], [[0.0, down], [bays, 0.0], [0.0, 0.0]]);
            }

            let up = [0.0, 1.0, 0.0];
            if n == 4 && area < 450.0 {
                // A simple house: pitched roof with the ridge along its longer side.
                let long = |i: usize| (b.ring[(i + 1) % 4].0 - b.ring[i].0).hypot(b.ring[(i + 1) % 4].1 - b.ring[i].1);
                let r = if long(0) >= long(1) { 0 } else { 1 };
                let q = |i: usize| b.ring[(i + r) % 4];
                let mid = |a: (f64, f64), c: (f64, f64)| ((a.0 + c.0) / 2.0, (a.1 + c.1) / 2.0);
                let ridge = top + (long(r + 1) * 0.32).min(3.2);
                let (m1, m2) = (at(mid(q(1), q(2)), ridge), at(mid(q(3), q(0)), ridge));
                let eave = |i: usize| at(q(i), top);
                for (a, c, ra, rc) in [(eave(0), eave(1), m2, m1), (eave(2), eave(3), m1, m2)] {
                    mesh.triangle([a, c, rc], up, [roof; 3], [PLAIN; 3]);
                    mesh.triangle([a, rc, ra], up, [roof; 3], [PLAIN; 3]);
                }
                // The triangular gable ends, in the wall colour.
                for (i, m) in [(1, m1), (3, m2)] {
                    let (a, c) = (q(i), q((i + 1) % 4));
                    mesh.triangle([at(a, top), at(c, top), m], [c.1 - a.1, 0.0, a.0 - c.0], [wall_top; 3], [PLAIN; 3]);
                }
            } else {
                for tri in triangulate(&b.ring) {
                    mesh.triangle(tri.map(|k| at(b.ring[k], top)), up, [roof; 3], [PLAIN; 3]);
                }
            }
        }
        mesh
    }
}

impl BuildingMesh {
    /// Adds a triangle facing the same way as `facing` (need not be exact or unit length).
    /// The stored normal is the triangle's true one, so sloped roofs shade correctly.
    fn triangle(&mut self, mut p: [[f64; 3]; 3], facing: [f64; 3], mut colors: [[f32; 3]; 3], mut uvs: [[f32; 2]; 3]) {
        let normal_of = |p: &[[f64; 3]; 3]| {
            let (u, v) = ([p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]], [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]]);
            [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
        };
        let mut n = normal_of(&p);
        // Wind every triangle anticlockwise as seen from outside, which is what the renderer treats as its front.
        if n[0] * facing[0] + n[1] * facing[1] + n[2] * facing[2] < 0.0 {
            p.swap(1, 2);
            colors.swap(1, 2);
            uvs.swap(1, 2);
            n = normal_of(&p);
        }
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        // Three points in a line (possible when triangulating an outline with redundant points) cover nothing.
        if len < 1e-6 {
            return;
        }
        for k in 0..3 {
            self.positions.extend(p[k].map(|v| v as f32));
            self.normals.extend(n.map(|v| (v / len) as f32));
            self.colors.extend(colors[k]);
            self.uvs.extend(uvs[k]);
        }
    }
}
