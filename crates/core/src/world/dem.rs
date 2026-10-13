//! Real-world terrain around the route, from "Terrarium" elevation tiles
//! (heights packed into RGB). Fetching the tiles is the app's job; choosing
//! which ones, decoding and sampling them happens here.

use std::collections::HashMap;

use crate::gpx::Route;

pub const TILE: usize = 256;
const MAX_TILES: usize = 64;
/// How far to either side of the road real terrain is wanted, in metres — far
/// enough to cover the mountains on the horizon.
const MARGIN: f64 = 6500.0;

#[derive(Default)]
pub struct Dem {
    pub zoom: u32,
    tiles: HashMap<(i64, i64), Vec<f32>>,
}

fn tile_xy(lat: f64, lon: f64, zoom: u32) -> (f64, f64) {
    let n = 2f64.powi(zoom as i32);
    let rad = lat.to_radians();
    ((lon + 180.0) / 360.0 * n, (1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
}

impl Dem {
    /// Picks the zoom level and tile set covering a corridor around the route.
    /// Returns `None` if even the coarsest useful zoom needs too many tiles.
    pub fn plan(route: &Route) -> Option<(u32, Vec<(i64, i64)>)> {
        let stride = ((300.0 / route.step).round() as usize).max(1);
        let offsets = [-MARGIN, -MARGIN / 2.0, 0.0, MARGIN / 2.0, MARGIN];
        for zoom in (9..=12).rev() {
            let mut keys: Vec<(i64, i64)> = Vec::new();
            for i in (0..route.n).step_by(stride) {
                for dx in offsets {
                    for dz in offsets {
                        let (lat, lon) = route.to_lat_lon(route.x[i] + dx, route.z[i] + dz);
                        let (tx, ty) = tile_xy(lat, lon, zoom);
                        keys.push((tx.floor() as i64, ty.floor() as i64));
                    }
                }
            }
            keys.sort_unstable();
            keys.dedup();
            if keys.len() <= MAX_TILES {
                return Some((zoom, keys));
            }
        }
        None
    }

    pub fn new(zoom: u32) -> Self {
        Self { zoom, tiles: HashMap::new() }
    }

    /// Adds a decoded tile. `pixels` is 256×256 with `channels` bytes per pixel (RGB or RGBA).
    pub fn insert_rgb(&mut self, key: (i64, i64), pixels: &[u8], channels: usize) -> bool {
        if channels < 3 || pixels.len() < TILE * TILE * channels {
            return false;
        }
        let heights = pixels
            .chunks_exact(channels)
            .take(TILE * TILE)
            .map(|p| p[0] as f32 * 256.0 + p[1] as f32 + p[2] as f32 / 256.0 - 32768.0)
            .collect();
        self.tiles.insert(key, heights);
        true
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    fn texel(&self, px: i64, py: i64) -> f64 {
        let t = TILE as i64;
        match self.tiles.get(&(px.div_euclid(t), py.div_euclid(t))) {
            Some(tile) => tile[(py.rem_euclid(t) * t + px.rem_euclid(t)) as usize] as f64,
            None => f64::NAN,
        }
    }

    /// Elevation in metres at a local (x, z) position, or NaN where no data was loaded.
    pub fn height_at(&self, route: &Route, x: f64, z: f64) -> f64 {
        let (lat, lon) = route.to_lat_lon(x, z);
        let (tx, ty) = tile_xy(lat, lon, self.zoom);
        let (fx, fy) = (tx * TILE as f64 - 0.5, ty * TILE as f64 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (u, v) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        (self.texel(x0, y0) * (1.0 - u) + self.texel(x0 + 1, y0) * u) * (1.0 - v) + (self.texel(x0, y0 + 1) * (1.0 - u) + self.texel(x0 + 1, y0 + 1) * u) * v
    }
}

/// Reconciles the route with the terrain model. Routes drawn in planners often
/// have no elevation at all, in which case it is filled in from the terrain.
/// Returns the vertical offset to add to terrain heights so they agree with the
/// route's own elevation on average.
pub fn align_dem(route: &mut Route, dem: &Dem) -> f64 {
    align_dem_with(route, dem, &[])
}

/// As `align_dem`, knowing which route samples are on a bridge or in a tunnel
/// (non-zero in `structures`; empty if unknown). It matters only when the route
/// has no elevation of its own and takes it from the terrain.
pub fn align_dem_with(route: &mut Route, dem: &Dem, structures: &[u8]) -> f64 {
    let mut sampled = vec![f32::NAN; route.n];
    let mut last = f32::NAN;
    let mut valid = 0;
    for i in 0..route.n {
        let h = dem.height_at(route, route.x[i], route.z[i]);
        if h.is_finite() {
            last = h as f32;
            valid += 1;
        }
        sampled[i] = last;
    }
    if (valid as f64) < route.n as f64 * 0.9 {
        return 0.0;
    }
    let first_valid = sampled.iter().position(|h| h.is_finite()).unwrap_or(0);
    let first = sampled[first_valid];
    sampled[..first_valid].fill(first);

    if !route.has_elevation {
        // The terrain goes over the hill a tunnel goes through, and down into the valley a
        // bridge goes across; the road does neither, and runs evenly from one end to the other.
        let mut from = 0;
        for i in 1..=route.n {
            if i < route.n && structures.get(i).copied().unwrap_or(0) == structures.get(from).copied().unwrap_or(0) {
                continue;
            }
            if structures.get(from).copied().unwrap_or(0) != 0 {
                // Anchored some thirty metres outside each end: a tunnel mouth is cut into a slope
                // that the coarse terrain grid smears across the road in front of it.
                let reach = ((30.0 / route.step).round() as usize).max(1);
                let (a, b) = (from.saturating_sub(reach), (i - 1 + reach).min(route.n - 1));
                for k in a..=b {
                    sampled[k] = sampled[a] + (sampled[b] - sampled[a]) * (k - a) as f32 / (b - a).max(1) as f32;
                }
            }
            from = i;
        }
        // Terrain grids are coarse (tens of metres per cell), which on a road cut into a
        // steep hillside reads as wild gradient swings — hence the much heavier smoothing.
        route.apply_elevation(sampled, 100.0);
        return 0.0;
    }
    let mut diffs: Vec<f64> = (0..route.n).step_by(10).map(|i| (route.ele[i] - sampled[i]) as f64).collect();
    diffs.sort_by(|a, b| a.total_cmp(b));
    diffs[diffs.len() / 2]
}
