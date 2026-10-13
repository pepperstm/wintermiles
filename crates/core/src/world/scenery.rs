//! Scenery from OpenStreetMap other than buildings: forests, water, and where
//! roads and railways run over bridges or through tunnels. This module holds
//! the map data as fetched, and works out what it means for a given route —
//! which lattice points are wooded or under water, which stretches of the
//! route are on a bridge or in a tunnel, and what passes overhead.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use super::height::{BRIDGE, OPEN, TUNNEL};
use super::mvt::{MapTile, MAP_ZOOM};
use super::{CELLS, CHUNK};
use crate::gpx::Route;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AreaKind {
    Forest,
    Water,
}

/// A mapped area as `[lat, lon]` rings. A point is in the area if an odd number
/// of rings enclose it, which takes care of clearings and islands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub kind: AreaKind,
    pub rings: Vec<Vec<[f64; 2]>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum WaterwayKind {
    River,
    Canal,
    Stream,
}

/// A watercourse mapped as a centre line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waterway {
    pub kind: WaterwayKind,
    pub line: Vec<[f64; 2]>,
}

/// A stretch of road or railway that is on a bridge or (if `tunnel`) in a tunnel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crossing {
    pub tunnel: bool,
    pub rail: bool,
    /// Overall width in metres.
    pub width: f32,
    pub line: Vec<[f64; 2]>,
}

/// The map data for a route's surroundings, as cached.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scenery {
    /// The vector tiles (x, y at `MAP_ZOOM`) this was read from; outside them nothing is known.
    pub tiles: Vec<[u32; 2]>,
    pub areas: Vec<Area>,
    pub waterways: Vec<Waterway>,
    pub crossings: Vec<Crossing>,
}

impl Scenery {
    /// Adds what one tile held. The tile's buildings are not scenery and are left for the caller.
    pub fn add(&mut self, tx: u32, ty: u32, tile: &mut MapTile) {
        self.tiles.push([tx, ty]);
        self.areas.append(&mut tile.areas);
        self.waterways.append(&mut tile.waterways);
        self.crossings.append(&mut tile.crossings);
    }
}

/// Lattice spacing: the terrain mesh's own vertex spacing.
const CELL: f64 = CHUNK / CELLS as f64;

/// One bit per terrain lattice point, stored per chunk.
#[derive(Default)]
pub(super) struct Mask {
    chunks: HashMap<(i32, i32), [u32; CELLS]>,
}

impl Mask {
    fn set(&mut self, i: i32, j: i32) {
        let n = CELLS as i32;
        self.chunks.entry((i.div_euclid(n), j.div_euclid(n))).or_insert([0; CELLS])[j.rem_euclid(n) as usize] |= 1 << i.rem_euclid(n);
    }

    /// Whether lattice point `(i, j)` — at `(i, j) × CELL` metres — is set.
    pub fn get(&self, i: i32, j: i32) -> bool {
        let n = CELLS as i32;
        self.chunks.get(&(i.div_euclid(n), j.div_euclid(n))).is_some_and(|rows| rows[j.rem_euclid(n) as usize] >> i.rem_euclid(n) & 1 == 1)
    }

    /// As `get`, for the lattice point nearest a position in metres.
    pub fn at(&self, x: f64, z: f64) -> bool {
        self.get((x / CELL).round() as i32, (z / CELL).round() as i32)
    }

    /// True if chunk `(cx, cz)` or any neighbour has a point set.
    pub fn touches(&self, cx: i32, cz: i32) -> bool {
        (cx - 1..=cx + 1).any(|ix| (cz - 1..=cz + 1).any(|iz| self.chunks.contains_key(&(ix, iz))))
    }

    /// Sets every lattice point inside the area bounded by `rings` (even–odd rule), by scanline.
    fn fill(&mut self, rings: &[Vec<(f64, f64)>]) {
        // Where each lattice row crosses the outline.
        let mut rows: HashMap<i32, Vec<f64>> = HashMap::new();
        for ring in rings {
            for k in 0..ring.len() {
                let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                let (lo, hi) = if a.1 <= b.1 { (a, b) } else { (b, a) };
                // Half-open in z, so a vertex shared by two edges counts once.
                for j in (lo.1 / CELL).ceil() as i32..(hi.1 / CELL).ceil() as i32 {
                    let t = (j as f64 * CELL - lo.1) / (hi.1 - lo.1);
                    rows.entry(j).or_default().push(lo.0 + (hi.0 - lo.0) * t);
                }
            }
        }
        for (j, mut xs) in rows {
            xs.sort_by(f64::total_cmp);
            for span in xs.chunks_exact(2) {
                for i in (span[0] / CELL).ceil() as i32..=(span[1] / CELL).floor() as i32 {
                    self.set(i, j);
                }
            }
        }
    }
}

/// A short piece of a watercourse drawn as a ribbon: its two ends, the sideways
/// direction at each (so pieces join without gaps at bends), and half its width.
pub(super) struct Strip {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub na: (f64, f64),
    pub nb: (f64, f64),
    pub half: f64,
}

/// Something carried over the route: a road or railway on a bridge, or the
/// ground above a tunnel too short to be worth boring through a hillside.
pub struct Overbridge {
    /// Centre line in local metres.
    pub line: Vec<(f64, f64)>,
    pub width: f64,
    /// Elevation of the underside.
    pub deck: f64,
    pub rail: bool,
}

/// What the scenery along a route amounts to, for the ride screen to report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct SceneryCounts {
    pub forests: usize,
    pub waters: usize,
    pub bridges: usize,
    pub tunnels: usize,
    pub overbridges: usize,
}

/// Scenery placed in a route's local frame.
#[derive(Default)]
pub struct Scene {
    pub(super) forest: Mask,
    pub(super) water: Mask,
    pub(super) strips: HashMap<(i32, i32), Vec<Strip>>,
    tiles: HashSet<(u32, u32)>,
    pub overbridges: Vec<Overbridge>,
    /// `OPEN`, `BRIDGE` or `TUNNEL` per route sample.
    pub kinds: Vec<u8>,
    /// Stretches of the route with something overhead, as `[from, to]` in metres along it.
    pub covered: Vec<[f64; 2]>,
    pub counts: SceneryCounts,
}

/// The route must run this close to a mapped bridge or tunnel, and this nearly parallel to it, to be on it.
const MATCH_M: f64 = 9.0;
const MATCH_COS: f64 = 0.85;
/// A tunnel shorter than this is drawn as passing under a bridge rather than through a hill.
const SHORT_TUNNEL_M: f64 = 50.0;
/// Clear height under anything that crosses overhead.
const HEADROOM: f64 = 5.4;
const HASH: f64 = 60.0;

fn bucket(x: f64, z: f64) -> (i32, i32) {
    ((x / HASH).floor() as i32, (z / HASH).floor() as i32)
}

/// Distance from a point to a line segment.
pub(super) fn to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dz) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dz) / (dx * dx + dz * dz).max(1e-12)).clamp(0.0, 1.0);
    (p.0 - a.0 - dx * t).hypot(p.1 - a.1 - dz * t)
}

/// Where segment `a→b` crosses `c→d`, as the fraction along `a→b`.
fn crossing(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> Option<f64> {
    let (r, s) = ((b.0 - a.0, b.1 - a.1), (d.0 - c.0, d.1 - c.1));
    let denom = r.0 * s.1 - r.1 * s.0;
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = ((c.0 - a.0) * s.1 - (c.1 - a.1) * s.0) / denom;
    let u = ((c.0 - a.0) * r.1 - (c.1 - a.1) * r.0) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// Runs of equal values as `(value, first index, last index)`.
pub(super) fn runs(kinds: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut out: Vec<(u8, usize, usize)> = Vec::new();
    for (i, &k) in kinds.iter().enumerate() {
        match out.last_mut() {
            Some(run) if run.0 == k => run.2 = i,
            _ => out.push((k, i, i)),
        }
    }
    out
}

/// Marks which route samples are on a mapped bridge or in a mapped tunnel.
fn classify(route: &Route, lines: &[(bool, Vec<(f64, f64)>)]) -> Vec<u8> {
    let mut hash: HashMap<(i32, i32), Vec<(usize, usize)>> = HashMap::new();
    for (l, (_, line)) in lines.iter().enumerate() {
        for s in 0..line.len().saturating_sub(1) {
            let (a, b) = (bucket(line[s].0, line[s].1), bucket(line[s + 1].0, line[s + 1].1));
            for ix in a.0.min(b.0)..=a.0.max(b.0) {
                for iz in a.1.min(b.1)..=a.1.max(b.1) {
                    hash.entry((ix, iz)).or_default().push((l, s));
                }
            }
        }
    }
    let mut kinds = vec![OPEN; route.n];
    if hash.is_empty() {
        return kinds;
    }
    for i in 0..route.n {
        let (before, after) = (i.saturating_sub(1), (i + 1).min(route.n - 1));
        let (tx, tz) = (route.x[after] - route.x[before], route.z[after] - route.z[before]);
        let p = (route.x[i], route.z[i]);
        let (cx, cz) = bucket(p.0, p.1);
        let mut best = MATCH_M;
        for ix in cx - 1..=cx + 1 {
            for iz in cz - 1..=cz + 1 {
                for &(l, s) in hash.get(&(ix, iz)).map_or(&[][..], |v| v) {
                    let (a, b) = (lines[l].1[s], lines[l].1[s + 1]);
                    let (dx, dz) = (b.0 - a.0, b.1 - a.1);
                    let parallel = (tx * dx + tz * dz).abs() / (tx.hypot(tz) * dx.hypot(dz)).max(1e-9);
                    let d = to_segment(p, a, b);
                    if parallel >= MATCH_COS && d < best {
                        best = d;
                        kinds[i] = if lines[l].0 { TUNNEL } else { BRIDGE };
                    }
                }
            }
        }
    }
    // GPS tracks wander: close short gaps in a bridge or tunnel, then drop anything left that is a single sample.
    let found = runs(&kinds);
    for w in found.windows(3) {
        if w[1].0 == OPEN && w[0].0 == w[2].0 && w[1].2 - w[1].1 < 3 {
            kinds[w[1].1..=w[1].2].fill(w[0].0);
        }
    }
    for (k, from, to) in runs(&kinds) {
        if k != OPEN && to == from {
            kinds[from] = OPEN;
        }
    }
    kinds
}

/// Joins lines that meet end to end. Tiles cut a bridge that spans their border in two.
fn join(mut lines: Vec<(Crossing, Vec<(f64, f64)>)>) -> Vec<(Crossing, Vec<(f64, f64)>)> {
    let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1) < 2.5;
    let mut i = 0;
    while i < lines.len() {
        let mut grew = false;
        for j in i + 1..lines.len() {
            if lines[i].0.rail != lines[j].0.rail {
                continue;
            }
            let (head, tail) = (lines[i].1[0], *lines[i].1.last().unwrap());
            let (other_head, other_tail) = (lines[j].1[0], *lines[j].1.last().unwrap());
            let mut other = lines[j].1.clone();
            if close(tail, other_head) {
                lines[i].1.extend(other);
            } else if close(tail, other_tail) {
                other.reverse();
                lines[i].1.extend(other);
            } else if close(head, other_tail) {
                other.extend(std::mem::take(&mut lines[i].1));
                lines[i].1 = other;
            } else if close(head, other_head) {
                other.reverse();
                other.extend(std::mem::take(&mut lines[i].1));
                lines[i].1 = other;
            } else {
                continue;
            }
            lines.swap_remove(j);
            grew = true;
            break;
        }
        if !grew {
            i += 1;
        }
    }
    lines
}

/// Which route samples run along a mapped bridge (`BRIDGE`) or tunnel (`TUNNEL`), from
/// their position alone — usable before the route has any elevation.
pub fn route_structures(route: &Route, scenery: &Scenery) -> Vec<u8> {
    let roads: Vec<(bool, Vec<(f64, f64)>)> =
        scenery.crossings.iter().filter(|c| !c.rail && c.line.len() >= 2).map(|c| (c.tunnel, c.line.iter().map(|p| route.to_local(p[0], p[1])).collect())).collect();
    classify(route, &roads)
}

impl Scene {
    pub fn new(route: &Route, scenery: &Scenery) -> Self {
        let local = |line: &[[f64; 2]]| line.iter().map(|p| route.to_local(p[0], p[1])).collect::<Vec<_>>();
        let mut scene = Scene { tiles: scenery.tiles.iter().map(|t| (t[0], t[1])).collect(), ..Default::default() };

        for area in &scenery.areas {
            let rings: Vec<Vec<(f64, f64)>> = area.rings.iter().map(|r| local(r)).collect();
            match area.kind {
                AreaKind::Forest => {
                    scene.forest.fill(&rings);
                    scene.counts.forests += 1;
                }
                AreaKind::Water => {
                    scene.water.fill(&rings);
                    scene.counts.waters += 1;
                }
            }
        }

        for way in &scenery.waterways {
            let half = match way.kind {
                WaterwayKind::River => 5.0,
                WaterwayKind::Canal => 3.5,
                WaterwayKind::Stream => 1.3,
            };
            scene.add_waterway(&local(&way.line), half);
            scene.counts.waters += 1;
        }

        // Roads only: a cyclist is never on the railway that happens to run alongside.
        scene.kinds = route_structures(route, scenery);

        // Short tunnels become open road passing under a slab, with banks either side.
        for (kind, from, to) in runs(&scene.kinds.clone()) {
            if kind != TUNNEL {
                continue;
            }
            let span = (to - from) as f64 * route.step;
            scene.covered.push([from as f64 * route.step, to as f64 * route.step]);
            if span >= SHORT_TUNNEL_M {
                continue;
            }
            scene.kinds[from..=to].fill(OPEN);
            let mid = (from + to) / 2;
            let (before, after) = (mid.saturating_sub(1), (mid + 1).min(route.n - 1));
            let (tx, tz) = (route.x[after] - route.x[before], route.z[after] - route.z[before]);
            let len = tx.hypot(tz).max(1e-9);
            let (rx, rz) = (tz / len * 9.0, -tx / len * 9.0);
            let top = route.ele[from..=to].iter().cloned().fold(f32::MIN, f32::max) as f64;
            scene.overbridges.push(Overbridge {
                line: vec![(route.x[mid] - rx, route.z[mid] - rz), (route.x[mid] + rx, route.z[mid] + rz)],
                width: span.max(8.0),
                deck: top + HEADROOM,
                rail: false,
            });
        }

        scene.add_overbridges(route, scenery.crossings.iter().filter(|c| !c.tunnel && c.line.len() >= 2).map(|c| (c.clone(), local(&c.line))).collect());

        for (kind, _, _) in runs(&scene.kinds) {
            match kind {
                BRIDGE => scene.counts.bridges += 1,
                TUNNEL => scene.counts.tunnels += 1,
                _ => {}
            }
        }
        scene.counts.overbridges = scene.overbridges.len();
        scene
    }

    fn add_waterway(&mut self, line: &[(f64, f64)], half: f64) {
        // Resample finely enough that the ribbon follows the terrain mesh under it.
        let mut points: Vec<(f64, f64)> = Vec::new();
        for w in line.windows(2) {
            let steps = ((w[1].0 - w[0].0).hypot(w[1].1 - w[0].1) / 2.5).ceil().max(1.0) as usize;
            for s in 0..steps {
                let t = s as f64 / steps as f64;
                points.push((w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t));
            }
        }
        points.extend(line.last());
        if points.len() < 2 {
            return;
        }
        let side = |i: usize| {
            let (a, b) = (points[i.saturating_sub(1)], points[(i + 1).min(points.len() - 1)]);
            let len = (b.0 - a.0).hypot(b.1 - a.1).max(1e-9);
            ((b.1 - a.1) / len, -(b.0 - a.0) / len)
        };
        for i in 0..points.len() - 1 {
            let (a, b) = (points[i], points[i + 1]);
            let key = (((a.0 + b.0) / 2.0 / CHUNK).floor() as i32, ((a.1 + b.1) / 2.0 / CHUNK).floor() as i32);
            self.strips.entry(key).or_default().push(Strip { a, b, na: side(i), nb: side(i + 1), half });
        }
    }

    /// Finds the bridges that cross over the route where it runs in the open.
    fn add_overbridges(&mut self, route: &Route, bridges: Vec<(Crossing, Vec<(f64, f64)>)>) {
        if bridges.is_empty() {
            return;
        }
        let mut hash: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for i in 0..route.n - 1 {
            hash.entry(bucket(route.x[i], route.z[i])).or_default().push(i);
        }
        for (bridge, mut line) in join(bridges) {
            let mut hit: Option<(usize, usize, f64)> = None;
            'search: for s in 0..line.len() - 1 {
                let (a, b) = (line[s], line[s + 1]);
                let (ca, cb) = (bucket(a.0, a.1), bucket(b.0, b.1));
                for ix in ca.0.min(cb.0) - 1..=ca.0.max(cb.0) + 1 {
                    for iz in ca.1.min(cb.1) - 1..=ca.1.max(cb.1) + 1 {
                        for &i in hash.get(&(ix, iz)).map_or(&[][..], |v| v) {
                            let (c, d) = ((route.x[i], route.z[i]), (route.x[i + 1], route.z[i + 1]));
                            let Some(t) = crossing(a, b, c, d) else { continue };
                            // Something that meets the route at a shallow angle is more likely the route's own bridge, mapped a little off.
                            let sin = ((b.0 - a.0) * (d.1 - c.1) - (b.1 - a.1) * (d.0 - c.0)).abs() / ((b.0 - a.0).hypot(b.1 - a.1) * (d.0 - c.0).hypot(d.1 - c.1)).max(1e-9);
                            // Nor does anything cross over the route at the ends of its own bridge: those are the
                            // footways and tram tracks that share it.
                            let around = i.saturating_sub(8)..(i + 9).min(route.n);
                            if sin >= 0.4 && self.kinds[around].iter().all(|&k| k == OPEN) {
                                hit = Some((s, i, t));
                                break 'search;
                            }
                        }
                    }
                }
            }
            let Some((s, i, t)) = hit else { continue };
            let at = (line[s].0 + (line[s + 1].0 - line[s].0) * t, line[s].1 + (line[s + 1].1 - line[s].1) * t);
            // The supports at either end must stand clear of the road beneath.
            let clear = bridge.width as f64 / 2.0 + 9.0;
            for end in [true, false] {
                let (tip, next) = if end { (line[line.len() - 1], line[line.len() - 2]) } else { (line[0], line[1]) };
                let reach = (tip.0 - at.0).hypot(tip.1 - at.1);
                if reach < clear {
                    let len = (tip.0 - next.0).hypot(tip.1 - next.1).max(1e-9);
                    let moved = (tip.0 + (tip.0 - next.0) / len * (clear - reach), tip.1 + (tip.1 - next.1) / len * (clear - reach));
                    if end {
                        *line.last_mut().unwrap() = moved;
                    } else {
                        line[0] = moved;
                    }
                }
            }
            let (from, to) = (i.saturating_sub(2), (i + 3).min(route.n - 1));
            let top = route.ele[from..=to].iter().cloned().fold(f32::MIN, f32::max) as f64;
            self.overbridges.push(Overbridge { line, width: bridge.width as f64, deck: top + HEADROOM, rail: bridge.rail });
        }
    }

    /// Whether map data was fetched for the place at local `(x, z)`.
    pub fn covers(&self, route: &Route, x: f64, z: f64) -> bool {
        if self.tiles.is_empty() {
            return false;
        }
        let (lat, lon) = route.to_lat_lon(x, z);
        let n = 2f64.powi(MAP_ZOOM as i32);
        let rad = lat.to_radians();
        let tile = ((lon + 180.0) / 360.0 * n, (1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / PI) / 2.0 * n);
        self.tiles.contains(&(tile.0.floor() as u32, tile.1.floor() as u32))
    }
}
