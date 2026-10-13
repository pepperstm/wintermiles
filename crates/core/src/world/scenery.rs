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

/// What sort of road, path or railway a mapped line is. In the order they are painted:
/// where two meet, the later one lies on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RoadKind {
    /// An unsurfaced footpath or bridleway.
    Path,
    /// A made-up footway, cycleway or flight of steps.
    Footway,
    /// A farm or forest track.
    Track,
    /// A pedestrian street.
    Pedestrian,
    /// Driveways, car-park aisles and the like.
    Service,
    /// Residential streets and country lanes.
    Minor,
    /// Primary and secondary roads, with lines painted on them.
    Main,
    /// Motorways and trunk roads.
    Major,
    Tram,
    Rail,
}

impl RoadKind {
    /// Width in metres of the surface: tarmac, gravel, or the ballast under a track.
    pub fn width(self) -> f64 {
        match self {
            RoadKind::Path => 1.2,
            RoadKind::Footway => 1.8,
            RoadKind::Track => 2.8,
            RoadKind::Pedestrian => 4.0,
            RoadKind::Service => 3.6,
            RoadKind::Minor => 6.0,
            RoadKind::Main => 7.5,
            RoadKind::Major => 10.5,
            RoadKind::Tram => 2.6,
            RoadKind::Rail => 3.4,
        }
    }

    pub fn is_rail(self) -> bool {
        matches!(self, RoadKind::Tram | RoadKind::Rail)
    }

    /// Whether the surface has an edge line each side and a broken one down the middle.
    pub(super) fn marked(self) -> bool {
        matches!(self, RoadKind::Main | RoadKind::Major)
    }

    /// Surface colour, in sRGB.
    pub(super) fn colour(self) -> [f64; 3] {
        match self {
            RoadKind::Path => [0.56, 0.50, 0.38],
            RoadKind::Footway => [0.60, 0.59, 0.56],
            RoadKind::Track => [0.53, 0.48, 0.38],
            RoadKind::Pedestrian => [0.64, 0.62, 0.58],
            RoadKind::Service => [0.33, 0.33, 0.35],
            RoadKind::Minor | RoadKind::Main | RoadKind::Major => [0.24, 0.25, 0.27],
            RoadKind::Tram => [0.38, 0.38, 0.39],
            RoadKind::Rail => [0.45, 0.41, 0.37],
        }
    }
}

/// A road, path or railway on the ground, as a `[lat, lon]` centre line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Road {
    pub kind: RoadKind,
    pub line: Vec<[f64; 2]>,
}

/// A stretch of road or railway that is on a bridge or (if `tunnel`) in a tunnel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crossing {
    pub tunnel: bool,
    pub kind: RoadKind,
    pub line: Vec<[f64; 2]>,
}

impl Crossing {
    /// Overall width in metres of a bridge that carries this: the surface plus a kerb and parapet each side.
    fn width(&self) -> f64 {
        self.kind.width() + 2.0
    }
}

/// The map data for a route's surroundings, as cached.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scenery {
    /// The vector tiles (x, y at `MAP_ZOOM`) this was read from; outside them nothing is known.
    pub tiles: Vec<[u32; 2]>,
    pub areas: Vec<Area>,
    pub waterways: Vec<Waterway>,
    pub crossings: Vec<Crossing>,
    /// Everything else in the road and rail network: the parts that simply lie on the ground.
    #[serde(default)]
    pub roads: Vec<Road>,
}

impl Scenery {
    /// Adds what one tile held. The tile's buildings are not scenery and are left for the caller.
    pub fn add(&mut self, tx: u32, ty: u32, tile: &mut MapTile) {
        self.tiles.push([tx, ty]);
        self.areas.append(&mut tile.areas);
        self.waterways.append(&mut tile.waterways);
        self.crossings.append(&mut tile.crossings);
        self.roads.append(&mut tile.roads);
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

/// A short piece of a road other than the route's own, to be laid on the terrain:
/// its corners (left and right at the start, right and left at the end, mitred at
/// bends) and how far along its road each end is.
pub(super) struct RoadStrip {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub corners: [(f64, f64); 4],
    pub along: (f64, f64),
    pub half: f64,
    pub kind: RoadKind,
    /// One of the two steel rails of a railway, rather than the bed it lies on.
    pub steel: bool,
    /// Close enough to the route that it may run into it.
    pub joins: bool,
}

/// A bridge on some other road, clear of the route.
pub struct Deck {
    /// Centre line in local metres, with points close enough together to follow the ground at each end.
    pub line: Vec<(f64, f64)>,
    pub kind: RoadKind,
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
#[serde(rename_all = "camelCase")]
pub struct SceneryCounts {
    pub forests: usize,
    pub waters: usize,
    pub bridges: usize,
    pub tunnels: usize,
    pub overbridges: usize,
    /// Other roads, paths and railways drawn, and how many of their bridges.
    pub roads: usize,
    pub road_bridges: usize,
}

/// Scenery placed in a route's local frame.
#[derive(Default)]
pub struct Scene {
    pub(super) forest: Mask,
    pub(super) water: Mask,
    pub(super) strips: HashMap<(i32, i32), Vec<Strip>>,
    road_strips: Vec<RoadStrip>,
    /// Per chunk, the road strips that reach into it, in painting order.
    road_index: HashMap<(i32, i32), Vec<u32>>,
    /// Route samples on a fine grid, for finding where other roads meet the route.
    route_cells: HashMap<(i32, i32), Vec<u32>>,
    pub decks: Vec<Deck>,
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
            if lines[i].0.kind != lines[j].0.kind {
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
        scenery.crossings.iter().filter(|c| !c.kind.is_rail() && c.line.len() >= 2).map(|c| (c.tunnel, c.line.iter().map(|p| route.to_local(p[0], p[1])).collect())).collect();
    classify(route, &roads)
}

/// A line with extra points put in so that none are more than `spacing` apart.
fn resample(line: &[(f64, f64)], spacing: f64) -> Vec<(f64, f64)> {
    let mut points: Vec<(f64, f64)> = Vec::new();
    for w in line.windows(2) {
        let steps = ((w[1].0 - w[0].0).hypot(w[1].1 - w[0].1) / spacing).ceil().max(1.0) as usize;
        for s in 0..steps {
            let t = s as f64 / steps as f64;
            points.push((w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t));
        }
    }
    points.extend(line.last());
    points
}

/// Answers "is this on, or across, the route?" for pieces of other roads.
struct Beside<'a> {
    route: &'a Route,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

impl<'a> Beside<'a> {
    const CELL: f64 = 15.0;

    fn new(route: &'a Route) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for i in 0..route.n {
            cells.entry(Self::cell(route.x[i], route.z[i])).or_default().push(i as u32);
        }
        Self { route, cells }
    }

    fn cell(x: f64, z: f64) -> (i32, i32) {
        ((x / Self::CELL).floor() as i32, (z / Self::CELL).floor() as i32)
    }

    /// The route sample nearest `p`, if one is within `reach` metres (at most a cell's width).
    fn nearest(&self, p: (f64, f64), reach: f64) -> Option<usize> {
        self.nearest_in(&self.cells, p, reach)
    }

    fn nearest_in(&self, cells: &HashMap<(i32, i32), Vec<u32>>, p: (f64, f64), reach: f64) -> Option<usize> {
        let (cx, cz) = Self::cell(p.0, p.1);
        let mut best = (reach * reach, None);
        for ix in cx - 1..=cx + 1 {
            for iz in cz - 1..=cz + 1 {
                for &i in cells.get(&(ix, iz)).map_or(&[][..], |v| v) {
                    let d2 = (self.route.x[i as usize] - p.0).powi(2) + (self.route.z[i as usize] - p.1).powi(2);
                    if d2 < best.0 {
                        best = (d2, Some(i as usize));
                    }
                }
            }
        }
        best.1
    }

    /// Whether the piece `a→b` of a mapped road runs along the route within `reach` metres: that is, is the route.
    fn same_road(&self, a: (f64, f64), b: (f64, f64), reach: f64) -> bool {
        let Some(i) = self.nearest(((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), reach) else { return false };
        let r = self.route;
        let (before, after) = (i.saturating_sub(1), (i + 1).min(r.n - 1));
        let (tx, tz) = (r.x[after] - r.x[before], r.z[after] - r.z[before]);
        let (dx, dz) = (b.0 - a.0, b.1 - a.1);
        (tx * dx + tz * dz).abs() >= 0.8 * tx.hypot(tz) * dx.hypot(dz)
    }

    /// Whether the piece `a→b` (no longer than a cell) cuts across the route.
    fn crosses(&self, a: (f64, f64), b: (f64, f64)) -> bool {
        let r = self.route;
        let (ca, cb) = (Self::cell(a.0, a.1), Self::cell(b.0, b.1));
        (ca.0.min(cb.0) - 1..=ca.0.max(cb.0) + 1).any(|ix| {
            (ca.1.min(cb.1) - 1..=ca.1.max(cb.1) + 1).any(|iz| {
                self.cells.get(&(ix, iz)).is_some_and(|list| list.iter().any(|&i| (i as usize) + 1 < r.n && crossing(a, b, (r.x[i as usize], r.z[i as usize]), (r.x[i as usize + 1], r.z[i as usize + 1])).is_some()))
            })
        })
    }
}

/// How close a mapped road must run to the route to be taken for the route itself. Footways
/// get less: a cycle path really does run alongside the carriageway.
const SAME_ROAD_M: f64 = 6.0;
const SAME_PATH_M: f64 = 3.5;
/// Gauge of a railway, and the width its rails are drawn (generously, so they show).
const GAUGE: f64 = 1.435;
const RAIL_HEAD: f64 = 0.16;

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

        let spare = scene.add_overbridges(route, scenery.crossings.iter().filter(|c| !c.tunnel && c.line.len() >= 2).map(|c| (c.clone(), local(&c.line))).collect());

        // The rest of the road network. The route's own road is in the map too, and is left out.
        let beside = Beside::new(route);
        for road in &scenery.roads {
            if scene.add_road(&beside, &local(&road.line), road.kind) {
                scene.counts.roads += 1;
            }
        }
        for (bridge, line) in spare {
            let line = resample(&line, 6.0);
            // Not the bridge the route itself is on, nor the footways and tram tracks that share it,
            // nor one that crosses the route too obliquely to have been made sense of above.
            let on_route = line.windows(2).any(|w| beside.same_road(w[0], w[1], 14.0) || beside.crosses(w[0], w[1]));
            let by_structure = line.iter().any(|&p| beside.nearest(p, 14.0).is_some_and(|i| scene.kinds[i] != OPEN));
            if line.len() >= 2 && !on_route && !by_structure {
                scene.decks.push(Deck { line, kind: bridge.kind });
            }
        }
        scene.counts.road_bridges = scene.decks.len();
        scene.route_cells = beside.cells;
        // Painting order within each chunk: lesser ways first, rails last.
        let strips = &scene.road_strips;
        for list in scene.road_index.values_mut() {
            list.sort_by_key(|&k| (strips[k as usize].kind, strips[k as usize].steel));
        }

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
        let points = resample(line, 2.5);
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

    /// Lays out one mapped road as strips. Returns false if nothing of it is drawn.
    fn add_road(&mut self, beside: &Beside, line: &[(f64, f64)], kind: RoadKind) -> bool {
        let points = resample(line, 20.0);
        if points.len() < 2 {
            return false;
        }
        let reach = if matches!(kind, RoadKind::Path | RoadKind::Footway) { SAME_PATH_M } else { SAME_ROAD_M };
        // Sideways at each point, lengthened at bends so the road keeps its width through them.
        let side = |i: usize| {
            let (p, q, r) = (points[i.saturating_sub(1)], points[i], points[(i + 1).min(points.len() - 1)]);
            let unit = |a: (f64, f64), b: (f64, f64)| {
                let len = (b.0 - a.0).hypot(b.1 - a.1).max(1e-9);
                ((b.0 - a.0) / len, (b.1 - a.1) / len)
            };
            let (u, v) = (if i == 0 { unit(q, r) } else { unit(p, q) }, if i + 1 == points.len() { unit(p, q) } else { unit(q, r) });
            let (tx, tz) = (u.0 + v.0, u.1 + v.1);
            let len = tx.hypot(tz).max(1e-9);
            let mitre = (2.0 / len).min(1.5);
            (tz / len * mitre, -tx / len * mitre)
        };
        // The bed, and for a railway the two rails on it.
        let mut ribbons = vec![(0.0, kind.width() / 2.0, false)];
        if kind.is_rail() {
            ribbons.extend([(-GAUGE / 2.0, RAIL_HEAD / 2.0, true), (GAUGE / 2.0, RAIL_HEAD / 2.0, true)]);
        }
        let (mut along, mut drawn) = (0.0, false);
        for i in 0..points.len() - 1 {
            let (a, b) = (points[i], points[i + 1]);
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            let ends = (along, along + len);
            along += len;
            if len < 1e-6 || (!kind.is_rail() && beside.same_road(a, b, reach)) {
                continue;
            }
            let (na, nb) = (side(i), side(i + 1));
            for &(offset, half, steel) in &ribbons {
                let at = |p: (f64, f64), n: (f64, f64), s: f64| (p.0 + n.0 * (offset + half * s), p.1 + n.1 * (offset + half * s));
                let corners = [at(a, na, -1.0), at(a, na, 1.0), at(b, nb, 1.0), at(b, nb, -1.0)];
                let index = self.road_strips.len() as u32;
                let joins = !kind.is_rail() && resample(&[a, b], 10.0).iter().any(|&p| beside.nearest(p, Beside::CELL).is_some());
                self.road_strips.push(RoadStrip { a, b, corners, along: ends, half: offset.abs() + half, kind, steel, joins });
                let chunk = |v: f64| (v / CHUNK).floor() as i32;
                let (xs, zs) = (corners.map(|c| chunk(c.0)), corners.map(|c| chunk(c.1)));
                for cx in *xs.iter().min().unwrap()..=*xs.iter().max().unwrap() {
                    for cz in *zs.iter().min().unwrap()..=*zs.iter().max().unwrap() {
                        self.road_index.entry((cx, cz)).or_default().push(index);
                    }
                }
            }
            drawn = true;
        }
        drawn
    }

    /// For a point on another road: how far it is from the centre line of the route where the
    /// route runs on the ground, and the route's elevation there. `None` if the route isn't near.
    pub(super) fn beside_route(&self, route: &Route, p: (f64, f64)) -> Option<(f64, f64)> {
        let i = Beside { route, cells: HashMap::new() }.nearest_in(&self.route_cells, p, Beside::CELL)?;
        if self.kinds[i] != OPEN {
            return None;
        }
        let mut best: Option<(f64, f64)> = None;
        for k in i.saturating_sub(1)..(i + 1).min(route.n - 1) {
            let (a, b) = ((route.x[k], route.z[k]), (route.x[k + 1], route.z[k + 1]));
            let (dx, dz) = (b.0 - a.0, b.1 - a.1);
            let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dz) / (dx * dx + dz * dz).max(1e-12)).clamp(0.0, 1.0);
            let d = (p.0 - a.0 - dx * t).hypot(p.1 - a.1 - dz * t);
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, route.ele[k] as f64 + (route.ele[k + 1] as f64 - route.ele[k] as f64) * t));
            }
        }
        best
    }

    /// The pieces of other roads that reach into chunk `(cx, cz)`, in the order to paint them.
    pub(super) fn roads_in(&self, cx: i32, cz: i32) -> impl Iterator<Item = &RoadStrip> + Clone {
        self.road_index.get(&(cx, cz)).map_or(&[][..], |v| v).iter().map(|&k| &self.road_strips[k as usize])
    }

    /// Finds the bridges that cross over the route where it runs in the open.
    /// Returns the bridges that turned out not to.
    fn add_overbridges(&mut self, route: &Route, bridges: Vec<(Crossing, Vec<(f64, f64)>)>) -> Vec<(Crossing, Vec<(f64, f64)>)> {
        let mut spare = Vec::new();
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
            let Some((s, i, t)) = hit else {
                spare.push((bridge, line));
                continue;
            };
            let at = (line[s].0 + (line[s + 1].0 - line[s].0) * t, line[s].1 + (line[s + 1].1 - line[s].1) * t);
            // The supports at either end must stand clear of the road beneath.
            let clear = bridge.width() / 2.0 + 9.0;
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
            self.overbridges.push(Overbridge { line, width: bridge.width(), deck: top + HEADROOM, rail: bridge.kind.is_rail() });
        }
        spare
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
