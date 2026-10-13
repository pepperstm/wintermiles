//! The height function behind the landscape. Close to the road the ground is
//! pinned to the route's own elevation (so the road never floats or tunnels);
//! further out it blends into real terrain data when available, or into
//! procedural hills when not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::dem::Dem;
use super::CHUNK;
use crate::gpx::Route;

/// Ground is flat at road level within this distance of the centreline…
const FLAT: f64 = 16.0;
/// …and fully blended into the surrounding terrain beyond this one.
pub(super) const BLEND: f64 = 90.0;
/// Another stretch of the route this close (a hairpin's other arm) shares the ground.
const SHARED: f64 = 18.0;
/// Within this distance of the centreline the ground follows the nearest stretch of road only.
const OWN_STRETCH: f64 = 46.0;
/// Samples further apart than this along the route count as a different stretch of road.
const OTHER_ARM_M: f64 = 25.0;
const HASH_CELL: f64 = 45.0;
const COARSE_CELL: f64 = 400.0;

/// Deterministic pseudo-random value in [0, 1) for an integer lattice point.
pub(super) fn hash2(ix: i32, iz: i32) -> f64 {
    let mut h = (ix as u32).wrapping_mul(374_761_393) ^ (iz as u32).wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f64 / 4_294_967_296.0
}

fn value_noise(x: f64, z: f64) -> f64 {
    let (ix, iz) = (x.floor(), z.floor());
    let (fx, fz) = (x - ix, z - iz);
    let (u, v) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
    let (ix, iz) = (ix as i32, iz as i32);
    let a = hash2(ix, iz);
    let b = hash2(ix.wrapping_add(1), iz);
    let c = hash2(ix, iz.wrapping_add(1));
    let d = hash2(ix.wrapping_add(1), iz.wrapping_add(1));
    a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v
}

/// Fractal noise in roughly [−1, 1].
pub(super) fn fbm(x: f64, z: f64) -> f64 {
    (value_noise(x, z) * 0.55 + value_noise(x * 2.1 + 17.0, z * 2.1 - 9.0) * 0.3 + value_noise(x * 4.3 - 31.0, z * 4.3 + 5.0) * 0.15) * 2.0 - 1.0
}

pub(super) fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn cell(v: f64, size: f64) -> i32 {
    (v / size).floor() as i32
}

pub struct HeightField {
    route: Arc<Route>,
    dem: Option<Dem>,
    dem_offset: f64,
    /// Route sample indices bucketed on a fine grid, for nearest-road queries.
    grid: HashMap<(i32, i32), Vec<u32>>,
    /// (sum of elevation, count) of route samples on a coarse grid.
    coarse: HashMap<(i32, i32), (f64, u32)>,
    /// Cache of the regional altitude on the chunk lattice. Chunks are built in parallel.
    lattice: Mutex<HashMap<(i32, i32), f64>>,
    mean_ele: f64,
}

impl HeightField {
    pub fn new(route: Arc<Route>, dem: Option<Dem>, dem_offset: f64) -> Self {
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        let mut coarse: HashMap<(i32, i32), (f64, u32)> = HashMap::new();
        let stride = ((10.0 / route.step).round() as usize).max(1);
        for i in (0..route.n).step_by(stride) {
            grid.entry((cell(route.x[i], HASH_CELL), cell(route.z[i], HASH_CELL))).or_default().push(i as u32);
            let c = coarse.entry((cell(route.x[i], COARSE_CELL), cell(route.z[i], COARSE_CELL))).or_insert((0.0, 0));
            c.0 += route.ele[i] as f64;
            c.1 += 1;
        }
        let mean_ele = route.ele.iter().map(|e| *e as f64).sum::<f64>() / route.n as f64;
        Self { route, dem, dem_offset, grid, coarse, lattice: Mutex::new(HashMap::new()), mean_ele }
    }

    /// A very smooth "regional altitude" derived from the route, sampled on a chunk-sized lattice.
    fn regional(&self, lx: i32, lz: i32) -> f64 {
        if let Some(v) = self.lattice.lock().unwrap().get(&(lx, lz)) {
            return *v;
        }
        let (x, z) = (lx as f64 * CHUNK, lz as f64 * CHUNK);
        let (cx, cz) = (cell(x, COARSE_CELL), cell(z, COARSE_CELL));
        // A faint pull toward the route's mean altitude keeps the surface continuous
        // where the route is too far away to have any say.
        let mut wsum = 1.0 / (3000.0f64 * 3000.0).powi(2);
        let mut hsum = wsum * self.mean_ele;
        for dx in -5..=5 {
            for dz in -5..=5 {
                let Some((sum, n)) = self.coarse.get(&(cx + dx, cz + dz)) else { continue };
                let ddx = ((cx + dx) as f64 + 0.5) * COARSE_CELL - x;
                let ddz = ((cz + dz) as f64 + 0.5) * COARSE_CELL - z;
                let w = *n as f64 / (ddx * ddx + ddz * ddz + 250.0 * 250.0).powi(2);
                wsum += w;
                hsum += w * sum / *n as f64;
            }
        }
        let v = hsum / wsum;
        self.lattice.lock().unwrap().insert((lx, lz), v);
        v
    }

    /// Terrain height ignoring the road: real elevation data if loaded, generated hills otherwise.
    pub fn surroundings(&self, x: f64, z: f64) -> f64 {
        if let Some(dem) = &self.dem {
            let h = dem.height_at(&self.route, x, z);
            if h.is_finite() {
                return h + self.dem_offset + fbm(x / 23.0, z / 23.0) * 0.8;
            }
        }
        let (lx, lz) = (cell(x, CHUNK), cell(z, CHUNK));
        let (u, v) = (x / CHUNK - lx as f64, z / CHUNK - lz as f64);
        let base = (self.regional(lx, lz) * (1.0 - u) + self.regional(lx + 1, lz) * u) * (1.0 - v)
            + (self.regional(lx, lz + 1) * (1.0 - u) + self.regional(lx + 1, lz + 1) * u) * v;
        base + fbm(x / 420.0, z / 420.0) * 70.0 + fbm(x / 90.0 + 40.0, z / 90.0) * 12.0 + fbm(x / 23.0, z / 23.0) * 1.5
    }

    /// Ground height at a point, and its distance to the road centreline (capped at the blend radius).
    pub fn height(&self, x: f64, z: f64) -> (f64, f64) {
        let route = &self.route;
        let (cx, cz) = (cell(x, HASH_CELL), cell(z, HASH_CELL));
        let (mut wsum, mut hsum) = (0.0f64, 0.0f64);
        let mut dmin2 = BLEND * BLEND;
        let mut nearest = 0usize;
        for ix in cx - 2..=cx + 2 {
            for iz in cz - 2..=cz + 2 {
                let Some(list) = self.grid.get(&(ix, iz)) else { continue };
                for &i in list {
                    let i = i as usize;
                    let (dx, dz) = (route.x[i] - x, route.z[i] - z);
                    let d2 = dx * dx + dz * dz;
                    if d2 >= BLEND * BLEND {
                        continue;
                    }
                    // Steep inverse-distance weighting: where two stretches of road pass close
                    // together at different heights (hairpins), each keeps its own level.
                    let w = 1.0 / (d2 + 9.0).powi(2);
                    wsum += w;
                    hsum += w * route.ele[i] as f64;
                    if d2 < dmin2 {
                        dmin2 = d2;
                        nearest = i;
                    }
                }
            }
        }
        let d = dmin2.sqrt();
        let around = self.surroundings(x, z);
        if wsum == 0.0 {
            return (around, d);
        }
        let mut road = hsum / wsum;
        // Close to the road, pin the ground to that stretch of road alone. On a hairpin
        // the two arms can be a few metres apart but several metres different in
        // height — closer than the mesh can resolve — so the ground between them
        // takes the lower arm's level: it can never bury a road, and the upper arm
        // stands on its retaining wall instead.
        if d < OWN_STRETCH {
            let apart = (OTHER_ARM_M / route.step) as usize;
            let (mut own_w, mut own_h) = (0.0f64, 0.0f64);
            // (squared distance, elevation) of the closest sample on a different stretch.
            let mut other = (SHARED * SHARED, f64::INFINITY);
            for ix in cx - 2..=cx + 2 {
                for iz in cz - 2..=cz + 2 {
                    let Some(list) = self.grid.get(&(ix, iz)) else { continue };
                    for &i in list {
                        let i = i as usize;
                        let (dx, dz) = (route.x[i] - x, route.z[i] - z);
                        let d2 = dx * dx + dz * dz;
                        // On a closed loop the finish runs straight into the start.
                        let along = i.abs_diff(nearest);
                        let along = if route.closed { along.min(route.n - along) } else { along };
                        if along <= apart {
                            let w = 1.0 / (d2 + 9.0).powi(2);
                            own_w += w;
                            own_h += w * route.ele[i] as f64;
                        } else if d2 < other.0 {
                            other = (d2, route.ele[i] as f64);
                        }
                    }
                }
            }
            let own = own_h / own_w;
            road = (own + (road - own) * smoothstep(FLAT, OWN_STRETCH, d)).min(other.1);
        }
        let t = smoothstep(FLAT, BLEND, d);
        ((road - 0.35) * (1.0 - t) + around * t, d)
    }
}
