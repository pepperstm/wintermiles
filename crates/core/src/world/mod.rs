//! Landscape generation. The UI only uploads these buffers to the GPU; all the
//! geometry — road ribbon, terrain chunks, the distant sheet — is computed here,
//! off the render thread.

mod buildings;
mod dem;
mod height;
mod mvt;

pub use buildings::{overpass_query, parse_overpass, BuildingMesh, Buildings, Footprint};
pub use dem::{align_dem, Dem, TILE};
pub use mvt::{building_tiles, parse_building_tile, BUILDING_ZOOM};
pub use height::HeightField;

use std::sync::Arc;

use serde::Serialize;

use crate::gpx::Route;
use height::{fbm, hash2, smoothstep, BLEND};

/// Side of a square terrain chunk in metres, and cells along each side.
pub const CHUNK: f64 = 240.0;
pub const CELLS: usize = 24;
/// The distant sheet: cells per side and cell size in metres.
pub const FAR_CELLS: usize = 96;
pub const FAR_CELL: f64 = 150.0;
/// The far sheet is at full height from here outward; chunks always reach beyond this.
const FAR_INNER: f64 = 1050.0;
pub const VIEW_CHUNKS: i32 = 6;
pub const ROAD_WIDTH: f64 = 6.5;
pub const ROAD_LIFT: f64 = 0.07;
const TREES_PER_CHUNK: i32 = 46;
/// Vertices across the road ribbon.
pub const ROAD_SECTION: usize = 6;
/// How far the retaining wall under the embankment reaches down. It is buried on
/// level ground and shows only where the terrain falls away (hairpins, cuttings).
const WALL_DEPTH: f64 = 14.0;

/// Layout constants the renderer needs to interpret the mesh buffers.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldLayout {
    pub road_section: usize,
    pub chunk: f64,
    pub cells: usize,
    pub far_cells: usize,
    pub far_cell: f64,
    pub view_chunks: i32,
    pub road_width: f64,
    pub road_lift: f64,
}

pub const LAYOUT: WorldLayout =
    WorldLayout { road_section: ROAD_SECTION, chunk: CHUNK, cells: CELLS, far_cells: FAR_CELLS, far_cell: FAR_CELL, view_chunks: VIEW_CHUNKS, road_width: ROAD_WIDTH, road_lift: ROAD_LIFT };

/// Vertex buffers for a grid mesh; the index pattern is fixed, so the renderer builds it.
#[derive(Default)]
pub struct GridMesh {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub colors: Vec<f32>,
    /// Trees as `[x, y, z, yaw, scale, height scale]` each.
    pub trees: Vec<f32>,
    pub buildings: BuildingMesh,
}

pub struct RoadMesh {
    /// `ROAD_SECTION` vertices per route sample, left to right: wall base, embankment
    /// foot, tarmac edge, tarmac edge, embankment foot, wall base.
    pub positions: Vec<f32>,
    pub uvs: Vec<f32>,
}

pub struct World {
    pub route: Arc<Route>,
    pub field: HeightField,
    pub buildings: Buildings,
    tree_line: f64,
}

impl World {
    pub fn new(route: Arc<Route>, dem: Option<Dem>, dem_offset: f64) -> Self {
        // Rough alpine tree line, falling with latitude.
        let tree_line = (3600.0 - route.lat0.abs() * 38.0).max(600.0);
        let field = HeightField::new(route.clone(), dem, dem_offset);
        Self { route, field, buildings: Buildings::default(), tree_line }
    }

    /// Adds buildings (OpenStreetMap footprints) to the landscape.
    pub fn with_buildings(mut self, footprints: &[Footprint]) -> Self {
        self.buildings = Buildings::new(&self.route, &self.field, footprints);
        self
    }

    /// Ground colour for a vertex: meadow, rock on steep faces, scree and snow up high.
    fn shade(&self, x: f64, z: f64, height: f64, ny: f64, road_distance: f64) -> [f32; 3] {
        let steep = smoothstep(0.35, 0.75, 1.0 - ny * ny);
        let high = smoothstep(self.tree_line + 200.0, self.tree_line + 900.0, height);
        let tint = fbm(x / 130.0, z / 130.0) * 0.5 + 0.5;
        let mut c = [0.3 + 0.16 * tint, 0.5 + 0.08 * tint, 0.2 + 0.04 * tint];
        let mut mix = |target: [f64; 3], amount: f64| {
            for k in 0..3 {
                c[k] += (target[k] - c[k]) * amount;
            }
        };
        mix([0.5, 0.48, 0.45], steep.max(high * 0.7));
        mix([0.95, 0.96, 1.0], smoothstep(self.tree_line + 1000.0, self.tree_line + 1500.0, height) * (1.0 - steep));
        // A worn verge beside the tarmac.
        mix([0.47, 0.44, 0.3], (1.0 - smoothstep(4.0, 13.0, road_distance)) * 0.6);
        // The palette is authored in sRGB; vertex colours are linear.
        c.map(|v| v.powf(2.2) as f32)
    }

    /// Builds the terrain chunk at chunk coordinates `(cx, cz)`.
    pub fn chunk(&self, cx: i32, cz: i32) -> GridMesh {
        let cell = CHUNK / CELLS as f64;
        let (x0, z0) = (cx as f64 * CHUNK, cz as f64 * CHUNK);
        // Heights on a grid padded by one cell, so normals match across chunk seams.
        let pad = CELLS + 3;
        let mut h = vec![0.0f64; pad * pad];
        let mut near = vec![0.0f64; pad * pad];
        for j in 0..pad {
            for i in 0..pad {
                let (height, d) = self.field.height(x0 + (i as f64 - 1.0) * cell, z0 + (j as f64 - 1.0) * cell);
                h[j * pad + i] = height;
                near[j * pad + i] = d;
            }
        }
        let row = CELLS + 1;
        let mut mesh = GridMesh {
            positions: Vec::with_capacity(row * row * 3),
            normals: Vec::with_capacity(row * row * 3),
            colors: Vec::with_capacity(row * row * 3),
            trees: Vec::new(),
            buildings: self.buildings.mesh(cx, cz),
        };
        for j in 0..row {
            for i in 0..row {
                let p = (j + 1) * pad + (i + 1);
                let (x, z) = (x0 + i as f64 * cell, z0 + j as f64 * cell);
                let nx = h[p - 1] - h[p + 1];
                let nz = h[p - pad] - h[p + pad];
                let len = (nx * nx + 4.0 * cell * cell + nz * nz).sqrt();
                let ny = 2.0 * cell / len;
                mesh.positions.extend_from_slice(&[x as f32, h[p] as f32, z as f32]);
                mesh.normals.extend_from_slice(&[(nx / len) as f32, ny as f32, (nz / len) as f32]);
                mesh.colors.extend_from_slice(&self.shade(x, z, h[p], ny, near[p]));
            }
        }

        // Trees: deterministic per chunk, clustered into woods by a low-frequency mask.
        for k in 0..TREES_PER_CHUNK {
            let tx = x0 + hash2(cx.wrapping_mul(131).wrapping_add(k), cz.wrapping_mul(17).wrapping_sub(k)) * CHUNK;
            let tz = z0 + hash2(cx.wrapping_mul(29).wrapping_sub(k), cz.wrapping_mul(113).wrapping_add(k * 7)) * CHUNK;
            let wood = fbm(tx / 260.0 + 9.0, tz / 260.0 - 4.0);
            if wood < -0.05 + hash2(k, cx.wrapping_add(cz)) * 0.25 {
                continue;
            }
            let (ty, d) = self.field.height(tx, tz);
            if d < 9.0 || ty > self.tree_line || self.buildings.covers(tx, tz) {
                continue;
            }
            // Skip cliffs: compare with the ground a few metres away.
            if (self.field.height(tx + 4.0, tz).0 - ty).abs() > 3.2 {
                continue;
            }
            let scale = 0.8 + hash2(k * 3, cx.wrapping_mul(7).wrapping_add(cz)) * 1.1;
            let yaw = hash2(k, cz) * std::f64::consts::TAU;
            mesh.trees.extend_from_slice(&[tx as f32, (ty - 0.2) as f32, tz as f32, yaw as f32, scale as f32, (scale * (0.85 + hash2(k, cx) * 0.4)) as f32]);
        }
        mesh
    }

    /// The distant landscape: one coarse sheet several kilometres wide centred on
    /// `(cx, cz)`. It is pushed down under the detailed chunks so the two never
    /// fight, leaving it visible only beyond them — as the mountains on the horizon.
    pub fn far(&self, cx: f64, cz: f64) -> GridMesh {
        let row = FAR_CELLS + 1;
        let half = FAR_CELLS as f64 / 2.0 * FAR_CELL;
        let at = |i: usize, j: usize| (cx - half + i as f64 * FAR_CELL, cz - half + j as f64 * FAR_CELL);
        let mut h = vec![0.0f64; row * row];
        for j in 0..row {
            for i in 0..row {
                let (x, z) = at(i, j);
                h[j * row + i] = self.field.surroundings(x, z);
            }
        }
        let mut mesh = GridMesh {
            positions: Vec::with_capacity(row * row * 3),
            normals: Vec::with_capacity(row * row * 3),
            colors: Vec::with_capacity(row * row * 3),
            trees: Vec::new(),
            buildings: BuildingMesh::default(),
        };
        for j in 0..row {
            for i in 0..row {
                let (x, z) = at(i, j);
                let nx = h[j * row + i.saturating_sub(1)] - h[j * row + (i + 1).min(row - 1)];
                let nz = h[j.saturating_sub(1) * row + i] - h[(j + 1).min(row - 1) * row + i];
                let len = (nx * nx + 4.0 * FAR_CELL * FAR_CELL + nz * nz).sqrt();
                let ny = 2.0 * FAR_CELL / len;
                let height = h[j * row + i];
                let sink = 1.0 - smoothstep(FAR_INNER - 160.0, FAR_INNER, (x - cx).hypot(z - cz));
                mesh.positions.extend_from_slice(&[x as f32, (height - 4.0 - sink * 60.0) as f32, z as f32]);
                mesh.normals.extend_from_slice(&[(nx / len) as f32, ny as f32, (nz / len) as f32]);
                mesh.colors.extend_from_slice(&self.shade(x, z, height, ny, BLEND));
            }
        }
        mesh
    }

    /// The road ribbon along the whole route.
    pub fn road(&self) -> RoadMesh {
        let r = &self.route;
        let n = r.n;
        let (edge, foot) = (ROAD_WIDTH / 2.0, ROAD_WIDTH / 2.0 + 2.6);
        let offsets = [-foot - 0.4, -foot, -edge, edge, foot, foot + 0.4];
        let drops = [-0.9 - WALL_DEPTH, -0.9, 0.0, 0.0, -0.9, -0.9 - WALL_DEPTH];
        // Texture columns: gravel shoulder on the outer 20/256ths, tarmac between.
        let us = [0.0f32, 0.0, 20.0 / 256.0, 236.0 / 256.0, 1.0, 1.0];
        let mut mesh = RoadMesh { positions: Vec::with_capacity(n * ROAD_SECTION * 3), uvs: Vec::with_capacity(n * ROAD_SECTION * 2) };
        for i in 0..n {
            let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
            let (mut tx, mut tz) = (r.x[b] - r.x[a], r.z[b] - r.z[a]);
            let len = tx.hypot(tz).max(1e-9);
            tx /= len;
            tz /= len;
            // Signed curvature (1/m), positive when the bend's centre is on the +offset side.
            // On the inside of a hairpin tighter than the road is wide, the edge would
            // cross over itself, so it is pulled in to just short of the bend's centre.
            let curvature = if i > 0 && i + 1 < n {
                ((r.x[i + 1] - 2.0 * r.x[i] + r.x[i - 1]) * tz - (r.z[i + 1] - 2.0 * r.z[i] + r.z[i - 1]) * tx) / (r.step * r.step)
            } else {
                0.0
            };
            let inside = if curvature.abs() > 1e-6 { 0.8 / curvature.abs() } else { f64::INFINITY };
            for k in 0..ROAD_SECTION {
                let offset = if offsets[k] * curvature > 0.0 { offsets[k].signum() * offsets[k].abs().min(inside) } else { offsets[k] };
                mesh.positions.extend_from_slice(&[(r.x[i] + tz * offset) as f32, (r.ele[i] as f64 + ROAD_LIFT + drops[k]) as f32, (r.z[i] - tx * offset) as f32]);
                mesh.uvs.extend_from_slice(&[us[k], (i as f64 * r.step / 12.0) as f32]);
            }
        }
        mesh
    }
}
