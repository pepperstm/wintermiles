//! Building outlines from OpenStreetMap's vector tiles (Mapbox Vector Tile
//! format, "Shortbread" schema) — the fallback when the Overpass API is
//! unavailable. The tiles carry outlines but no heights, so heights are
//! estimated from footprint size.

use std::f64::consts::PI;

use super::buildings::Footprint;
use super::height::hash2;
use crate::gpx::Route;

/// The zoom level at which the tiles include every building.
pub const BUILDING_ZOOM: u32 = 14;
const LAYER: &str = "buildings";
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

/// Decodes a polygon feature's drawing commands into closed rings in tile units.
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

/// Extracts the building outlines from one vector tile.
pub fn parse_building_tile(bytes: &[u8], zoom: u32, tx: u32, ty: u32) -> Option<Vec<Footprint>> {
    let mut found = Vec::new();
    let mut tile = Reader::new(bytes);
    while !tile.done() {
        let (field, wire) = tile.key()?;
        if (field, wire) != (3, 2) {
            tile.skip(wire)?;
            continue;
        }
        // A layer: collect its name, extent and raw features before interpreting them.
        let mut layer = Reader::new(tile.bytes()?);
        let (mut name, mut extent, mut features) = ("", 4096.0f64, Vec::new());
        while !layer.done() {
            let (field, wire) = layer.key()?;
            match (field, wire) {
                (1, 2) => name = std::str::from_utf8(layer.bytes()?).ok()?,
                (2, 2) => features.push(layer.bytes()?),
                (5, 0) => extent = layer.varint()? as f64,
                _ => layer.skip(wire)?,
            }
        }
        if name != LAYER {
            continue;
        }
        let n = 2f64.powi(zoom as i32);
        let to_lat_lon = |p: (i64, i64)| {
            let (fx, fy) = (tx as f64 + p.0 as f64 / extent, ty as f64 + p.1 as f64 / extent);
            [(PI * (1.0 - 2.0 * fy / n)).sinh().atan().to_degrees(), fx / n * 360.0 - 180.0]
        };
        // Metres per tile unit here, for sizing footprints.
        let lat = to_lat_lon((0, extent as i64 / 2))[0];
        let unit_m = 40_075_016.7 * lat.to_radians().cos() / n / extent;

        for (index, feature) in features.into_iter().enumerate() {
            let mut f = Reader::new(feature);
            let (mut kind, mut geometry) = (0, &[][..]);
            while !f.done() {
                let (field, wire) = f.key()?;
                match (field, wire) {
                    (3, 0) => kind = f.varint()?,
                    (4, 2) => geometry = f.bytes()?,
                    _ => f.skip(wire)?,
                }
            }
            if kind != POLYGON {
                continue;
            }
            for (k, ring) in rings(geometry)?.into_iter().enumerate() {
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
                let mut outline: Vec<[f64; 2]> = ring.iter().map(|p| to_lat_lon(*p)).collect();
                outline.push(outline[0]);
                found.push(Footprint { id, height: estimated_height(area * unit_m * unit_m, (id ^ (id >> 32)) as i32), ring: outline });
            }
        }
    }
    Some(found)
}

/// The vector tiles (x, y at `BUILDING_ZOOM`) covering a corridor `radius_m` either side of the route.
pub fn building_tiles(route: &Route, radius_m: f64) -> Vec<(u32, u32)> {
    let n = 2f64.powi(BUILDING_ZOOM as i32);
    let stride = ((100.0 / route.step).round() as usize).max(1);
    let mut tiles = Vec::new();
    for i in (0..route.n).step_by(stride).chain([route.n - 1]) {
        for dx in [-radius_m, 0.0, radius_m] {
            for dz in [-radius_m, 0.0, radius_m] {
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
