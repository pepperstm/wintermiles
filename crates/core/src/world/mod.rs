//! Landscape generation. The UI only uploads these buffers to the GPU; all the
//! geometry — road ribbon, terrain chunks, the distant sheet — is computed here,
//! off the render thread.

mod buildings;
mod dem;
mod height;
mod mesh;
mod mvt;
mod scenery;
mod structures;

pub use buildings::{overpass_query, parse_overpass, BuildingMesh, Buildings, Footprint};
pub use dem::{align_dem, align_dem_with, Dem, TILE};
pub use height::{HeightField, Probe, BRIDGE, OPEN, TUNNEL};
pub use mesh::TriMesh;
pub use mvt::{corridor_tiles, parse_building_tile, parse_tile, route_corridor, MapTile, MAP_ZOOM};
pub use scenery::{route_structures, Area, AreaKind, Crossing, Overbridge, Scene, Scenery, SceneryCounts, Waterway, WaterwayKind};
pub use structures::Structures;

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
/// The share of lattice cells in a mapped forest that get a tree.
const FOREST_DENSITY: f64 = 0.6;
/// Vertices across the road ribbon.
pub const ROAD_SECTION: usize = 6;
/// How far the retaining wall under the embankment reaches down. It is buried on
/// level ground and shows only where the terrain falls away (hairpins, cuttings).
const WALL_DEPTH: f64 = 14.0;
/// Thickness of a bridge deck.
pub(crate) const DECK_DEPTH: f64 = 1.6;

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
    /// Water surfaces as loose triangles: positions, and a colour per vertex.
    pub water: Vec<f32>,
    pub water_colors: Vec<f32>,
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
    pub scene: Scene,
    tree_line: f64,
}

/// Depth of water over a lake or river bed, and how far its surface lies below the banks.
const WATER_DEPTH: f64 = 1.6;
const WATER_DROP: f64 = 0.5;
// Water colours, authored in sRGB: open water, and the shallows at the edge.
const WATER: [f64; 3] = [0.17, 0.36, 0.47];
const SHALLOWS: [f64; 3] = [0.30, 0.50, 0.55];
const FOREST_FLOOR: [f64; 3] = [0.20, 0.33, 0.16];
const BED: [f64; 3] = [0.36, 0.36, 0.30];

fn linear(c: [f64; 3]) -> [f32; 3] {
    c.map(|v| v.powf(2.2) as f32)
}

/// Height of the terrain mesh inside a padded chunk grid at `(fx, fz)` cells from its
/// corner, interpolated over the same two triangles per cell that the renderer draws.
fn on_mesh(grid: &[f64], pad: usize, fx: f64, fz: f64) -> f64 {
    let top = (pad - 1) as f64 - 1e-6;
    let (fx, fz) = (fx.clamp(0.0, top), fz.clamp(0.0, top));
    let (i, j) = (fx as usize, fz as usize);
    let (u, v) = (fx - i as f64, fz - j as f64);
    let at = |di: usize, dj: usize| grid[(j + dj) * pad + i + di];
    if u + v <= 1.0 {
        at(0, 0) + (at(1, 0) - at(0, 0)) * u + (at(0, 1) - at(0, 0)) * v
    } else {
        at(1, 1) + (at(0, 1) - at(1, 1)) * (1.0 - u) + (at(1, 0) - at(1, 1)) * (1.0 - v)
    }
}

impl World {
    pub fn new(route: Arc<Route>, dem: Option<Dem>, dem_offset: f64) -> Self {
        // Rough alpine tree line, falling with latitude.
        let tree_line = (3600.0 - route.lat0.abs() * 38.0).max(600.0);
        let field = HeightField::new(route.clone(), dem, dem_offset);
        Self { route, field, buildings: Buildings::default(), scene: Scene::default(), tree_line }
    }

    /// A world with OpenStreetMap scenery (forests, water, bridges, tunnels) and buildings.
    /// The scenery comes first: bridges and tunnels reshape the ground the buildings stand on.
    pub fn with_map(route: Arc<Route>, dem: Option<Dem>, dem_offset: f64, scenery: Option<&Scenery>, footprints: &[Footprint]) -> Self {
        let Some(scenery) = scenery else { return Self::new(route, dem, dem_offset).with_buildings(footprints) };
        let scene = Scene::new(&route, scenery);
        let tree_line = (3600.0 - route.lat0.abs() * 38.0).max(600.0);
        let field = HeightField::with_kinds(route.clone(), dem, dem_offset, scene.kinds.clone());
        Self { route, field, buildings: Buildings::default(), scene, tree_line }.with_buildings(footprints)
    }

    /// Adds buildings (OpenStreetMap footprints) to the landscape.
    pub fn with_buildings(mut self, footprints: &[Footprint]) -> Self {
        self.buildings = Buildings::new(&self.route, &self.field, footprints);
        self
    }

    /// Ground colour for a vertex: meadow, rock on steep faces, scree and snow up high.
    fn shade(&self, x: f64, z: f64, height: f64, ny: f64, road_distance: f64, cover: Option<([f64; 3], f64)>) -> [f32; 3] {
        let steep = smoothstep(0.35, 0.75, 1.0 - ny * ny);
        let high = smoothstep(self.tree_line + 200.0, self.tree_line + 900.0, height);
        let tint = fbm(x / 130.0, z / 130.0) * 0.5 + 0.5;
        let mut c = [0.3 + 0.16 * tint, 0.5 + 0.08 * tint, 0.2 + 0.04 * tint];
        let mut mix = |target: [f64; 3], amount: f64| {
            for k in 0..3 {
                c[k] += (target[k] - c[k]) * amount;
            }
        };
        if let Some((colour, amount)) = cover {
            mix(colour, amount);
        }
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
        let scene = &self.scene;
        // Water needs real terrain to lie in: on generated hills a lake would be draped over the slopes.
        let watery = self.field.has_dem() && scene.water.touches(cx, cz);
        // Heights on a grid padded by a margin: one cell so that normals match across chunk
        // seams, and more where there is water, whose level is settled between neighbours.
        let m = if watery { 3 } else { 1 };
        let pad = CELLS + 1 + 2 * m;
        // Lattice coordinates of the padded grid's corner.
        let (li0, lj0) = (cx * CELLS as i32 - m as i32, cz * CELLS as i32 - m as i32);
        let mut h = vec![0.0f64; pad * pad];
        let mut near = vec![0.0f64; pad * pad];
        let mut open = vec![0.0f64; pad * pad];
        let mut trench = vec![false; pad * pad];
        // Where water stands (0 to 1: it shallows to nothing against a road), and the level of its surface.
        let mut lake = vec![0.0f64; pad * pad];
        let mut level = vec![f64::INFINITY; pad * pad];
        for j in 0..pad {
            for i in 0..pad {
                let (li, lj) = (li0 + i as i32, lj0 + j as i32);
                let mapped_water = watery && scene.water.get(li, lj);
                // The fine roughness of the ground is left out at the water's edge, or the shoreline would wander with it.
                let shore = watery && (mapped_water || (-1..=1).any(|di| (-1..=1).any(|dj| scene.water.get(li + di, lj + dj))));
                let p = self.field.probe(x0 + (i as f64 - m as f64) * cell, z0 + (j as f64 - m as f64) * cell, shore);
                let k = j * pad + i;
                h[k] = p.ground;
                near[k] = p.road;
                open[k] = p.open;
                trench[k] = p.trench;
                // A road that crosses water without a mapped bridge stays on a causeway.
                if mapped_water && !p.trench {
                    lake[k] = smoothstep(4.0, 12.0, p.open);
                    level[k] = p.surface - WATER_DROP;
                }
            }
        }
        let wet: Vec<bool> = lake.iter().map(|&l| l > 0.0).collect();
        if watery {
            let around = |k: usize| {
                let (i, j) = (k % pad, k / pad);
                (j.saturating_sub(1)..=(j + 1).min(pad - 1)).flat_map(move |jj| (i.saturating_sub(1)..=(i + 1).min(pad - 1)).map(move |ii| jj * pad + ii))
            };
            // Terrain data is coarse and a mapped shoreline exact, so water near a steep bank would
            // start part-way up it. Each point takes the lowest level nearby: still water finds its own.
            for _ in 0..2 {
                let before = level.clone();
                for k in (0..pad * pad).filter(|&k| wet[k]) {
                    level[k] = around(k).filter(|&n| wet[n]).map(|n| before[n]).fold(before[k], f64::min);
                }
            }
            for k in (0..pad * pad).filter(|&k| wet[k]) {
                h[k] = h[k].min(level[k] + WATER_DROP - WATER_DEPTH * lake[k]);
            }
            // The sheet of water runs on to the first dry point beyond the shore, at the water's
            // level — or just under the ground there, if the ground is lower (the face of a dam).
            let settled = level.clone();
            for k in (0..pad * pad).filter(|&k| !wet[k]) {
                level[k] = around(k).filter(|&n| wet[n]).map(|n| settled[n]).fold(f64::INFINITY, f64::min).min(h[k] - 0.3);
            }
        }
        let row = CELLS + 1;
        let mut mesh = GridMesh {
            positions: Vec::with_capacity(row * row * 3),
            normals: Vec::with_capacity(row * row * 3),
            colors: Vec::with_capacity(row * row * 3),
            buildings: self.buildings.mesh(cx, cz),
            ..Default::default()
        };
        for j in 0..row {
            for i in 0..row {
                let p = (j + m) * pad + (i + m);
                let (x, z) = (x0 + i as f64 * cell, z0 + j as f64 * cell);
                let nx = h[p - 1] - h[p + 1];
                let nz = h[p - pad] - h[p + pad];
                let len = (nx * nx + 4.0 * cell * cell + nz * nz).sqrt();
                let ny = 2.0 * cell / len;
                let cover = if wet[p] {
                    Some((BED, 0.8))
                } else if scene.forest.get(li0 + (m + i) as i32, lj0 + (m + j) as i32) {
                    Some((FOREST_FLOOR, 0.7))
                } else {
                    None
                };
                mesh.positions.extend_from_slice(&[x as f32, h[p] as f32, z as f32]);
                mesh.normals.extend_from_slice(&[(nx / len) as f32, ny as f32, (nz / len) as f32]);
                mesh.colors.extend_from_slice(&self.shade(x, z, h[p], ny, near[p], cover));
            }
        }

        // Lakes and wide rivers: a sheet over every cell that touches water, on the same
        // triangles as the ground, so the two meet in a clean shoreline.
        if watery {
            let (deep, shallow) = (linear(WATER), linear(SHALLOWS));
            for j in 0..CELLS {
                for i in 0..CELLS {
                    let corners = [(i, j), (i, j + 1), (i + 1, j), (i + 1, j), (i, j + 1), (i + 1, j + 1)].map(|(ci, cj)| ((cj + m) * pad + ci + m, ci, cj));
                    if !corners.iter().any(|c| wet[c.0]) || corners.iter().any(|c| trench[c.0]) {
                        continue;
                    }
                    for (k, ci, cj) in corners {
                        mesh.water.extend_from_slice(&[(x0 + ci as f64 * cell) as f32, level[k] as f32, (z0 + cj as f64 * cell) as f32]);
                        mesh.water_colors.extend_from_slice(if wet[k] { &deep } else { &shallow });
                    }
                }
            }
        }
        // Rivers and streams mapped as lines: ribbons laid on the terrain mesh.
        if let Some(strips) = scene.strips.get(&(cx, cz)).filter(|_| self.field.has_dem()) {
            let colour = linear(WATER);
            for s in strips {
                let mid = ((s.a.0 + s.b.0) / 2.0, (s.a.1 + s.b.1) / 2.0);
                let k = (((mid.1 - z0) / cell).round() as usize + m).min(pad - 1) * pad + (((mid.0 - x0) / cell).round() as usize + m).min(pad - 1);
                // Not where a lake already covers it, nor across the road (a culvert), nor over a tunnel.
                if wet[k] || trench[k] || open[k] < 7.0 {
                    continue;
                }
                let corner = |p: (f64, f64), n: (f64, f64), side: f64| {
                    let (x, z) = (p.0 + n.0 * s.half * side, p.1 + n.1 * s.half * side);
                    [x as f32, (on_mesh(&h, pad, (x - x0) / cell + m as f64, (z - z0) / cell + m as f64) + 0.3) as f32, z as f32]
                };
                let (a0, a1, b0, b1) = (corner(s.a, s.na, -1.0), corner(s.a, s.na, 1.0), corner(s.b, s.nb, -1.0), corner(s.b, s.nb, 1.0));
                // Wound to face upward, like the lake sheets.
                for p in [a0, b0, a1, b0, b1, a1] {
                    mesh.water.extend_from_slice(&p);
                    mesh.water_colors.extend_from_slice(&colour);
                }
            }
        }

        let streams = scene.strips.get(&(cx, cz)).map_or(&[][..], |v| v);
        // Where a tree would stand: `None` if the spot is on the road, in water, in a house, on a cliff or above the tree line.
        let plant = |tx: f64, tz: f64| -> Option<f64> {
            let (fx, fz) = ((tx - x0) / cell + m as f64, (tz - z0) / cell + m as f64);
            let k = (fz as usize).min(pad - 2) * pad + (fx as usize).min(pad - 2);
            let corners = [k, k + 1, k + pad, k + pad + 1];
            if corners.iter().any(|&c| wet[c]) || streams.iter().any(|s| scenery::to_segment((tx, tz), s.a, s.b) < s.half + 1.5) {
                return None;
            }
            // Over a tunnel the mesh is the floor of its trench; the trees belong on the land above.
            let (ty, d) = if corners.iter().any(|&c| trench[c]) { self.field.surface(tx, tz) } else { (on_mesh(&h, pad, fx, fz), on_mesh(&near, pad, fx, fz)) };
            let drop = corners.iter().map(|&c| h[c]).fold(f64::MIN, f64::max) - corners.iter().map(|&c| h[c]).fold(f64::MAX, f64::min);
            (d >= 9.0 && ty <= self.tree_line && (drop <= 8.0 || corners.iter().any(|&c| trench[c])) && !self.buildings.covers(tx, tz)).then_some(ty)
        };
        let mapped = scene.covers(&self.route, x0 + CHUNK / 2.0, z0 + CHUNK / 2.0);

        // Mapped forest: a tree in most cells of the lattice.
        if scene.forest.touches(cx, cz) {
            for j in 0..CELLS as i32 {
                for i in 0..CELLS as i32 {
                    let (li, lj) = (li0 + m as i32 + i, lj0 + m as i32 + j);
                    if !scene.forest.get(li, lj) || hash2(li.wrapping_mul(3), lj.wrapping_mul(5)) > FOREST_DENSITY {
                        continue;
                    }
                    let tx = x0 + (i as f64 + hash2(li, lj.wrapping_mul(7))) * cell;
                    let tz = z0 + (j as f64 + hash2(li.wrapping_mul(11), lj)) * cell;
                    let Some(ty) = plant(tx, tz) else { continue };
                    let scale = 1.0 + hash2(li, lj) * 0.9;
                    let yaw = hash2(lj, li) * std::f64::consts::TAU;
                    mesh.trees.extend_from_slice(&[tx as f32, (ty - 0.2) as f32, tz as f32, yaw as f32, scale as f32, (scale * (0.9 + hash2(li.wrapping_add(lj), li) * 0.4)) as f32]);
                }
            }
        }

        // Elsewhere: deterministic per chunk, clustered into woods by a low-frequency mask.
        // Where the map says what is forest and what is not, only a scattering is kept.
        for k in 0..TREES_PER_CHUNK {
            if mapped && k % 3 != 0 {
                continue;
            }
            let tx = x0 + hash2(cx.wrapping_mul(131).wrapping_add(k), cz.wrapping_mul(17).wrapping_sub(k)) * CHUNK;
            let tz = z0 + hash2(cx.wrapping_mul(29).wrapping_sub(k), cz.wrapping_mul(113).wrapping_add(k * 7)) * CHUNK;
            let wood = fbm(tx / 260.0 + 9.0, tz / 260.0 - 4.0);
            if wood < -0.05 + hash2(k, cx.wrapping_add(cz)) * 0.25 || scene.forest.at(tx, tz) {
                continue;
            }
            let Some(ty) = plant(tx, tz) else { continue };
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
            ..Default::default()
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
                mesh.colors.extend_from_slice(&self.shade(x, z, height, ny, BLEND, None));
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
        // A bridge deck has a kerb and a slab edge in place of an embankment and retaining wall.
        let deck = [-DECK_DEPTH, -0.25, 0.0, 0.0, -0.25, -DECK_DEPTH];
        let kinds = self.field.kinds();
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
                mesh.positions.extend_from_slice(&[(r.x[i] + tz * offset) as f32, (r.ele[i] as f64 + ROAD_LIFT + if kinds[i] == BRIDGE { deck[k] } else { drops[k] }) as f32, (r.z[i] - tx * offset) as f32]);
                mesh.uvs.extend_from_slice(&[us[k], (i as f64 * r.step / 12.0) as f32]);
            }
        }
        mesh
    }
}
