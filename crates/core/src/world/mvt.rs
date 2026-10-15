//! OpenStreetMap's vector tiles (Mapbox Vector Tile format, "Shortbread"
//! schema): the source for forests, water, roads, bridges and tunnels, and the
//! fallback for buildings when the Overpass API is unavailable. The tiles
//! carry building outlines but no heights, so those are estimated from
//! footprint size.

use std::f64::consts::PI;

use super::buildings::Footprint;
use super::scenery::{Area, AreaKind, Crossing, Road, RoadKind, Waterway, WaterwayKind};
use super::height::hash2;
use crate::gpx::Route;

/// The zoom level at which the tiles include every building.
pub const MAP_ZOOM: u32 = 14;
const POINT: u64 = 1;
const LINE: u64 = 2;
const POLYGON: u64 = 3;

/// Minimal protobuf reader: just enough to walk a vector tile.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.buf.get(self.pos)?;
            self.pos += 1;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.varint()? as usize;
        let out = self.buf.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(out)
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Next `(field number, wire type)`; `None` if the data is cut short.
    fn key(&mut self) -> Option<(u64, u64)> {
        self.varint().map(|k| (k >> 3, k & 7))
    }

    fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.advance(8),
            2 => self.bytes().map(|_| ()),
            5 => self.advance(4),
            _ => None,
        }
    }

    fn advance(&mut self, n: usize) -> Option<()> {
        self.pos = self.pos.checked_add(n).filter(|p| *p <= self.buf.len())?;
        Some(())
    }
}

fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Decodes a feature's drawing commands into paths in tile units: closed rings for a polygon, open lines for a line.
fn rings(geometry: &[u8]) -> Option<Vec<Vec<(i64, i64)>>> {
    let mut r = Reader::new(geometry);
    let (mut x, mut y) = (0i64, 0i64);
    let mut out: Vec<Vec<(i64, i64)>> = Vec::new();
    while !r.done() {
        let command = r.varint()?;
        let (id, count) = (command & 7, command >> 3);
        match id {
            // MoveTo starts a ring, LineTo extends it.
            1 | 2 => {
                for _ in 0..count {
                    x += zigzag(r.varint()?);
                    y += zigzag(r.varint()?);
                    if id == 1 {
                        out.push(Vec::new());
                    }
                    out.last_mut()?.push((x, y));
                }
            }
            // ClosePath: the ring implicitly returns to its first point.
            7 => {}
            _ => return None,
        }
    }
    Some(out)
}

fn ring_area(ring: &[(i64, i64)]) -> f64 {
    let n = ring.len();
    (0..n).map(|i| (ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1) as f64).sum::<f64>() / 2.0
}

/// With no mapped height to go on, bigger footprints are assumed to be taller buildings.
fn estimated_height(area_m2: f64, seed: i32) -> f32 {
    let base = match area_m2 {
        a if a < 40.0 => 3.0,
        a if a < 220.0 => 6.5,
        a if a < 700.0 => 8.5,
        _ => 10.0,
    };
    (base * (0.88 + hash2(seed, 11) * 0.24)) as f32
}

/// Everything of interest in one vector tile.
#[derive(Debug, Default, PartialEq)]
pub struct MapTile {
    pub footprints: Vec<Footprint>,
    pub areas: Vec<Area>,
    pub waterways: Vec<Waterway>,
    pub crossings: Vec<Crossing>,
    pub roads: Vec<Road>,
    pub labels: Vec<Label>,
}

/// A named place or point of interest.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Label {
    /// "city", "town", "village", "hamlet", "suburb", "locality", or a point of interest:
    /// "cafe", "restaurant", "pub", "water", "bike_shop", "viewpoint", "church", "castle",
    /// "monument", "museum", "hut", "attraction".
    pub kind: String,
    pub name: String,
    /// `[lat, lon]`.
    pub at: [f64; 2],
}

/// The kind of point of interest a `pois` feature is, if it is one worth showing.
fn poi_kind(amenity: &str, tourism: &str, shop: &str, historic: &str) -> Option<&'static str> {
    Some(match (amenity, tourism, shop, historic) {
        ("cafe" | "ice_cream", ..) => "cafe",
        ("restaurant" | "fast_food", ..) => "restaurant",
        ("pub" | "bar" | "biergarten", ..) => "pub",
        ("drinking_water" | "water_point" | "fountain", ..) => "water",
        ("place_of_worship", ..) => "church",
        (_, "viewpoint", ..) => "viewpoint",
        (_, "museum", ..) => "museum",
        (_, "alpine_hut" | "wilderness_hut", ..) => "hut",
        (_, "attraction", ..) => "attraction",
        (_, _, "bicycle", _) => "bike_shop",
        (.., "castle" | "fort" | "ruins") => "castle",
        (.., "monument" | "memorial") => "monument",
        _ => return None,
    })
}

/// An attribute value. Numbers are of no interest here.
#[derive(Clone, Copy, PartialEq)]
enum Value<'a> {
    Text(&'a str),
    Flag(bool),
    Other,
}

fn value(bytes: &[u8]) -> Option<Value<'_>> {
    let mut r = Reader::new(bytes);
    let mut out = Value::Other;
    while !r.done() {
        match r.key()? {
            (1, 2) => out = Value::Text(std::str::from_utf8(r.bytes()?).ok()?),
            (7, 0) => out = Value::Flag(r.varint()? != 0),
            (_, wire) => r.skip(wire)?,
        }
    }
    Some(out)
}

struct Feature<'a> {
    kind: u64,
    geometry: &'a [u8],
    /// Alternating indices into the layer's key and value tables.
    tags: Vec<u64>,
}

struct Layer<'a> {
    name: &'a str,
    extent: f64,
    keys: Vec<&'a str>,
    values: Vec<Value<'a>>,
    features: Vec<Feature<'a>>,
}

impl<'a> Layer<'a> {
    fn read(bytes: &'a [u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let mut layer = Layer { name: "", extent: 4096.0, keys: Vec::new(), values: Vec::new(), features: Vec::new() };
        while !r.done() {
            match r.key()? {
                (1, 2) => layer.name = std::str::from_utf8(r.bytes()?).ok()?,
                (2, 2) => {
                    let mut f = Reader::new(r.bytes()?);
                    let mut feature = Feature { kind: 0, geometry: &[], tags: Vec::new() };
                    while !f.done() {
                        match f.key()? {
                            (2, 2) => {
                                let mut tags = Reader::new(f.bytes()?);
                                while !tags.done() {
                                    feature.tags.push(tags.varint()?);
                                }
                            }
                            (3, 0) => feature.kind = f.varint()?,
                            (4, 2) => feature.geometry = f.bytes()?,
                            (_, wire) => f.skip(wire)?,
                        }
                    }
                    layer.features.push(feature);
                }
                (3, 2) => layer.keys.push(std::str::from_utf8(r.bytes()?).ok()?),
                (4, 2) => layer.values.push(value(r.bytes()?)?),
                (5, 0) => layer.extent = r.varint()? as f64,
                (_, wire) => r.skip(wire)?,
            }
        }
        Some(layer)
    }

    fn attr(&self, f: &Feature, name: &str) -> Value<'a> {
        f.tags
            .chunks_exact(2)
            .find(|t| self.keys.get(t[0] as usize) == Some(&name))
            .and_then(|t| self.values.get(t[1] as usize).copied())
            .unwrap_or(Value::Other)
    }

    fn text(&self, f: &Feature, name: &str) -> &'a str {
        match self.attr(f, name) {
            Value::Text(s) => s,
            _ => "",
        }
    }

    fn flag(&self, f: &Feature, name: &str) -> bool {
        self.attr(f, name) == Value::Flag(true)
    }
}

/// The parts of a line that lie inside the tile. Tiles overlap at their edges, so
/// without this a river or bridge near a border would be drawn by both neighbours.
fn clip(path: &[(i64, i64)], extent: f64) -> Vec<Vec<(f64, f64)>> {
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    for pair in path.windows(2) {
        let (a, b) = ((pair[0].0 as f64, pair[0].1 as f64), (pair[1].0 as f64, pair[1].1 as f64));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        // Liang–Barsky: narrow [t0, t1] to the part of the segment inside each edge in turn.
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut inside = true;
        for (p, q) in [(-dx, a.0), (dx, extent - a.0), (-dy, a.1), (dy, extent - a.1)] {
            if p == 0.0 {
                inside &= q >= 0.0;
            } else if p < 0.0 {
                t0 = t0.max(q / p);
            } else {
                t1 = t1.min(q / p);
            }
        }
        if !inside || t0 >= t1 {
            continue;
        }
        let (from, to) = ((a.0 + dx * t0, a.1 + dy * t0), (a.0 + dx * t1, a.1 + dy * t1));
        match out.last_mut() {
            Some(line) if line.last() == Some(&from) => line.push(to),
            _ => out.push(vec![from, to]),
        }
    }
    out
}

/// Drops points closer than `spacing` tile units to the one before, always keeping the ends.
fn thin(path: &[(f64, f64)], spacing: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(path.len());
    for (i, p) in path.iter().enumerate() {
        match out.last() {
            Some(last) if i + 1 < path.len() && (p.0 - last.0).hypot(p.1 - last.1) < spacing => {}
            _ => out.push(*p),
        }
    }
    out
}

/// Drops the points a line can do without: those within `tolerance` tile units of the straight
/// line between the points kept either side (Douglas–Peucker). A straight street becomes two points.
fn simplify(path: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if path.len() < 3 {
        return path.to_vec();
    }
    let mut keep = vec![false; path.len()];
    keep[0] = true;
    keep[path.len() - 1] = true;
    let mut spans = vec![(0, path.len() - 1)];
    while let Some((from, to)) = spans.pop() {
        let (a, b) = (path[from], path[to]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy).max(1e-12);
        // Distance from the chord, or from its start if the span doubles back on itself.
        let off = |p: (f64, f64)| if len < 1e-9 { (p.0 - a.0).hypot(p.1 - a.1) } else { ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len };
        let Some((worst, distance)) = (from + 1..to).map(|k| (k, off(path[k]))).max_by(|x, y| x.1.total_cmp(&y.1)) else { continue };
        if distance > tolerance {
            keep[worst] = true;
            spans.extend([(from, worst), (worst, to)]);
        }
    }
    path.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// What the tiles call `kind`, as something to draw. `None` for what isn't drawn (runways, roads being built).
fn road_kind(kind: &str, link: bool, rail: bool) -> Option<RoadKind> {
    Some(match kind.trim_end_matches("_link") {
        "tram" | "light_rail" => RoadKind::Tram,
        _ if rail => RoadKind::Rail,
        // A slip road is one carriageway of an ordinary main road.
        "motorway" | "trunk" if link || kind.ends_with("_link") => RoadKind::Main,
        "motorway" | "trunk" => RoadKind::Major,
        "primary" | "secondary" => RoadKind::Main,
        "tertiary" | "unclassified" | "residential" | "living_street" | "busway" => RoadKind::Minor,
        "service" => RoadKind::Service,
        "pedestrian" => RoadKind::Pedestrian,
        "track" => RoadKind::Track,
        "footway" | "cycleway" | "steps" => RoadKind::Footway,
        "path" | "bridleway" => RoadKind::Path,
        _ => return None,
    })
}

/// Extracts buildings, forests, water, and roads and railways with their bridges and tunnels from one vector tile.
pub fn parse_tile(bytes: &[u8], zoom: u32, tx: u32, ty: u32) -> Option<MapTile> {
    let mut found = MapTile::default();
    let mut tile = Reader::new(bytes);
    while !tile.done() {
        let (field, wire) = tile.key()?;
        if (field, wire) != (3, 2) {
            tile.skip(wire)?;
            continue;
        }
        let layer = Layer::read(tile.bytes()?)?;
        if !matches!(layer.name, "buildings" | "land" | "water_polygons" | "water_lines" | "streets" | "place_labels" | "pois") {
            continue;
        }
        let extent = layer.extent;
        let n = 2f64.powi(zoom as i32);
        let to_lat_lon = |p: (f64, f64)| {
            let (fx, fy) = (tx as f64 + p.0 / extent, ty as f64 + p.1 / extent);
            // Seven decimal places is about a centimetre, and keeps the cached file compact.
            [(PI * (1.0 - 2.0 * fy / n)).sinh().atan().to_degrees(), fx / n * 360.0 - 180.0].map(|v| (v * 1e7).round() / 1e7)
        };
        // Metres per tile unit here, for sizing things.
        let lat = to_lat_lon((0.0, extent / 2.0))[0];
        let unit_m = 40_075_016.7 * lat.to_radians().cos() / n / extent;
        let float = |ring: &[(i64, i64)]| ring.iter().map(|p| (p.0 as f64, p.1 as f64)).collect::<Vec<_>>();

        for (index, f) in layer.features.iter().enumerate() {
            match (layer.name, f.kind) {
                ("buildings", POLYGON) => {
                    for (k, ring) in rings(f.geometry)?.into_iter().enumerate() {
                        // Outer rings have positive area in tile coordinates; negative ones are courtyards.
                        let area = ring_area(&ring);
                        if ring.len() < 3 || area <= 0.0 {
                            continue;
                        }
                        // Tiles overlap at their edges, so a building near a border shows up in both.
                        // Keep it only in the tile that holds its centre.
                        let cx = ring.iter().map(|p| p.0 as f64).sum::<f64>() / ring.len() as f64;
                        let cy = ring.iter().map(|p| p.1 as f64).sum::<f64>() / ring.len() as f64;
                        if !(0.0..extent).contains(&cx) || !(0.0..extent).contains(&cy) {
                            continue;
                        }
                        let id = ((tx as u64) << 40) ^ ((ty as u64) << 20) ^ ((index as u64) << 4) ^ k as u64;
                        let mut outline: Vec<[f64; 2]> = float(&ring).into_iter().map(to_lat_lon).collect();
                        outline.push(outline[0]);
                        found.footprints.push(Footprint { id, height: estimated_height(area * unit_m * unit_m, (id ^ (id >> 32)) as i32), ring: outline });
                    }
                }
                ("land" | "water_polygons", POLYGON) => {
                    let kind = match (layer.name, layer.text(f, "kind")) {
                        ("land", "forest") => AreaKind::Forest,
                        ("water_polygons", _) => AreaKind::Water,
                        _ => continue,
                    };
                    // Clearings and islands arrive as extra rings; the area is whatever an odd number of rings encloses.
                    let outlines: Vec<Vec<[f64; 2]>> = rings(f.geometry)?
                        .iter()
                        .map(|ring| thin(&float(ring), 4.0 / unit_m))
                        .filter(|ring| ring.len() >= 3)
                        .map(|ring| ring.into_iter().map(to_lat_lon).collect())
                        .collect();
                    if !outlines.is_empty() {
                        found.areas.push(Area { kind, rings: outlines });
                    }
                }
                ("water_lines", LINE) => {
                    let kind = match layer.text(f, "kind") {
                        "river" => WaterwayKind::River,
                        "canal" => WaterwayKind::Canal,
                        "stream" => WaterwayKind::Stream,
                        _ => continue,
                    };
                    // Piped and culverted stretches are out of sight.
                    if layer.flag(f, "tunnel") {
                        continue;
                    }
                    for path in rings(f.geometry)? {
                        for piece in clip(&path, extent) {
                            found.waterways.push(Waterway { kind, line: thin(&piece, 2.0 / unit_m).into_iter().map(to_lat_lon).collect() });
                        }
                    }
                }
                ("place_labels" | "pois", POINT) => {
                    let name = layer.text(f, "name").trim();
                    let kind = if layer.name == "place_labels" {
                        match layer.text(f, "kind") {
                            k @ ("city" | "town" | "village" | "hamlet" | "suburb" | "locality") => k,
                            _ => continue,
                        }
                    } else {
                        match poi_kind(layer.text(f, "amenity"), layer.text(f, "tourism"), layer.text(f, "shop"), layer.text(f, "historic")) {
                            Some(k) => k,
                            None => continue,
                        }
                    };
                    // Drinking water is worth showing unnamed; anything else needs a name to say.
                    if name.is_empty() && kind != "water" {
                        continue;
                    }
                    let Some(&p) = rings(f.geometry)?.first().and_then(|r| r.first()) else { continue };
                    let p = (p.0 as f64, p.1 as f64);
                    // Labels near an edge appear in the neighbouring tile too; keep the one that holds it.
                    if !(0.0..extent).contains(&p.0) || !(0.0..extent).contains(&p.1) {
                        continue;
                    }
                    found.labels.push(Label { kind: kind.into(), name: name.chars().take(48).collect(), at: to_lat_lon(p) });
                }
                ("streets", LINE) => {
                    let Some(kind) = road_kind(layer.text(f, "kind"), layer.flag(f, "link"), layer.flag(f, "rail")) else { continue };
                    let (bridge, tunnel) = (layer.flag(f, "bridge"), layer.flag(f, "tunnel"));
                    let oneway = layer.flag(f, "oneway") || layer.flag(f, "oneway_reverse");
                    for path in rings(f.geometry)? {
                        for piece in clip(&path, extent) {
                            match (bridge, tunnel) {
                                (false, false) => found.roads.push(Road { kind, oneway, line: simplify(&piece, 0.15 / unit_m).into_iter().map(to_lat_lon).collect() }),
                                // A covered bridge is as good as out of sight.
                                (true, true) => {}
                                _ => found.crossings.push(Crossing { tunnel, kind, line: piece.into_iter().map(to_lat_lon).collect() }),
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Some(found)
}

/// Extracts just the building outlines from one vector tile.
pub fn parse_building_tile(bytes: &[u8], zoom: u32, tx: u32, ty: u32) -> Option<Vec<Footprint>> {
    parse_tile(bytes, zoom, tx, ty).map(|t| t.footprints)
}

/// The vector tiles (x, y at `MAP_ZOOM`) covering a corridor `radius_m` either side of the route.
pub fn corridor_tiles(route: &Route, radius_m: f64) -> Vec<(u32, u32)> {
    let n = 2f64.powi(MAP_ZOOM as i32);
    let stride = ((100.0 / route.step).round() as usize).max(1);
    let mut tiles = Vec::new();
    for i in (0..route.n).step_by(stride).chain([route.n - 1]) {
        // Probe points no further apart than the smallest tile, so none is stepped over.
        let offsets = [-radius_m, -radius_m / 2.0, 0.0, radius_m / 2.0, radius_m];
        for dx in offsets {
            for dz in offsets {
                let (lat, lon) = route.to_lat_lon(route.x[i] + dx, route.z[i] + dz);
                let rad = lat.to_radians();
                let x = (lon + 180.0) / 360.0 * n;
                let y = (1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / PI) / 2.0 * n;
                tiles.push((x.floor() as u32, y.floor() as u32));
            }
        }
    }
    tiles.sort_unstable();
    tiles.dedup();
    tiles
}

/// A quick test for "roughly within `radius_m` of the route", by `(lat, lon)`. It errs
/// on the generous side, by up to about twice the radius.
pub fn route_corridor(route: &Route, radius_m: f64) -> impl Fn(f64, f64) -> bool + '_ {
    let cell = move |x: f64, z: f64| ((x / radius_m).floor() as i32, (z / radius_m).floor() as i32);
    let stride = ((radius_m / 4.0 / route.step) as usize).max(1);
    let near: std::collections::HashSet<(i32, i32)> = (0..route.n).step_by(stride).map(|i| cell(route.x[i], route.z[i])).collect();
    move |lat, lon| {
        let (x, z) = route.to_local(lat, lon);
        let (cx, cz) = cell(x, z);
        (cx - 1..=cx + 1).any(|ix| (cz - 1..=cz + 1).any(|iz| near.contains(&(ix, iz))))
    }
}
