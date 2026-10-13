//! Lays the roads, paths and railways around the route onto the terrain. Each
//! piece is cut along the terrain mesh's own triangles, so it lies exactly on
//! the ground however that folds, rather than floating over dips or cutting
//! through rises.

use super::mesh::TriMesh;
use super::height::smoothstep;
use super::scenery::RoadStrip;
use super::{CELLS, ROAD_LIFT, ROAD_WIDTH};

/// How far the surface sits above the ground. The renderer also draws roads without
/// writing depth, in the order given, so they never fight the ground or each other.
const LIFT: f64 = 0.05;
/// Across the texture: a marked road spans the left of it, and unmarked surfaces sample a blank part.
const MARKED_SPAN: f64 = 0.375;
const BLANK: f64 = 0.9;
/// Metres of road per repeat of the texture, which is one dash of centre line and the gap after it.
const DASH_PERIOD: f64 = 12.0;
const STEEL: [f64; 3] = [0.20, 0.19, 0.19];
/// Where a road meets the route it is cut into slices this long, so that it can be shaped to the junction.
const SLICE: f64 = 1.0;
/// A side road comes up from the verge to the route's tarmac over this distance from its edge…
const RAMP: f64 = 3.0;
/// …and runs this far onto it, as the mouth of the junction.
const MOUTH: f64 = 0.5;

/// A point in a cell: position in cell units from its corner, then texture coordinates.
type Point = [f64; 4];

/// A chunk's ground as built: heights on its padded grid, normals on its own vertices.
pub(super) struct Ground<'a> {
    pub x0: f64,
    pub z0: f64,
    pub cell: f64,
    /// Margin of the padded grid, in cells.
    pub m: usize,
    pub pad: usize,
    pub h: &'a [f64],
    pub normals: &'a [f32],
    pub wet: &'a [bool],
    pub trench: &'a [bool],
    /// For a point: its distance from the route's centre line where the route is on the ground
    /// nearby, and the route's elevation there.
    pub route: &'a dyn Fn((f64, f64)) -> Option<(f64, f64)>,
}

/// Keeps the part of a polygon where `inside` is not negative (Sutherland–Hodgman, one edge).
fn cut(polygon: &[Point], inside: impl Fn(&Point) -> f64, out: &mut Vec<Point>) {
    out.clear();
    for k in 0..polygon.len() {
        let (a, b) = (polygon[k], polygon[(k + 1) % polygon.len()]);
        let (da, db) = (inside(&a), inside(&b));
        if da >= 0.0 {
            out.push(a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            let t = da / (da - db);
            out.push([0, 1, 2, 3].map(|c| a[c] + (b[c] - a[c]) * t));
        }
    }
}

impl Ground<'_> {
    /// Adds the part of `strip` that lies in this chunk to `mesh`.
    pub fn lay(&self, mesh: &mut TriMesh, strip: &RoadStrip) {
        if !strip.joins {
            return self.lay_piece(mesh, strip, 0.0, 1.0, false);
        }
        // Near the route: in short slices, leaving out those that would lie under the route's own tarmac.
        let c = &strip.corners;
        let slices = ((strip.along.1 - strip.along.0) / SLICE).ceil().max(1.0);
        for k in 0..slices as usize {
            let (from, to) = (k as f64 / slices, (k + 1) as f64 / slices);
            let t = (from + to) / 2.0;
            let middle = ((c[0].0 + c[1].0) / 2.0 * (1.0 - t) + (c[3].0 + c[2].0) / 2.0 * t, (c[0].1 + c[1].1) / 2.0 * (1.0 - t) + (c[3].1 + c[2].1) / 2.0 * t);
            if (self.route)(middle).is_none_or(|(d, _)| d >= ROAD_WIDTH / 2.0 - MOUTH) {
                self.lay_piece(mesh, strip, from, to, true);
            }
        }
    }

    /// Lays the part of a strip between two fractions of its length.
    fn lay_piece(&self, mesh: &mut TriMesh, strip: &RoadStrip, start: f64, end: f64, shaped: bool) {
        let colour = if strip.steel { STEEL } else if strip.kind.marked() { [1.0; 3] } else { strip.kind.colour() }.map(|v| v.powf(2.2) as f32);
        let (left, right) = if strip.kind.marked() && !strip.steel { (0.0, MARKED_SPAN) } else { (BLANK, BLANK) };
        let c = &strip.corners;
        let between = |a: (f64, f64), b: (f64, f64), t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let along = |t: f64| (strip.along.0 + (strip.along.1 - strip.along.0) * t) / DASH_PERIOD;
        // In cells from the chunk's corner.
        let quad: [Point; 4] = [(between(c[0], c[3], start), left, along(start)), (between(c[1], c[2], start), right, along(start)), (between(c[1], c[2], end), right, along(end)), (between(c[0], c[3], end), left, along(end))]
            .map(|(p, s, t)| [(p.0 - self.x0) / self.cell, (p.1 - self.z0) / self.cell, s, t]);
        let span = |c: usize| {
            let (lo, hi) = quad.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p[c]), hi.max(p[c])));
            (lo.floor().max(0.0) as usize, (hi.floor() as isize).min(CELLS as isize - 1))
        };
        let ((i0, i1), (j0, j1)) = (span(0), span(1));
        let row = CELLS + 1;
        let (mut work, mut next) = (Vec::with_capacity(12), Vec::with_capacity(12));
        for j in j0 as isize..=j1 {
            for i in i0 as isize..=i1 {
                let (i, j) = (i as usize, j as usize);
                let (fi, fj) = (i as f64, j as f64);
                let at = |di: usize, dj: usize| (j + dj + self.m) * self.pad + i + di + self.m;
                // The cell's two triangles, as the renderer splits it: by which side of the diagonal u + v = 1.
                for lower in [true, false] {
                    let corners = if lower { [at(0, 0), at(1, 0), at(0, 1)] } else { [at(1, 1), at(0, 1), at(1, 0)] };
                    // Not into a tunnel's trench, nor along the bottom of a lake.
                    if corners.iter().any(|&c| self.trench[c]) || corners.iter().all(|&c| self.wet[c]) {
                        continue;
                    }
                    let sign = if lower { 1.0 } else { -1.0 };
                    cut(&quad, |p| p[0] - fi, &mut work);
                    cut(&work, |p| fi + 1.0 - p[0], &mut next);
                    cut(&next, |p| p[1] - fj, &mut work);
                    cut(&work, |p| fj + 1.0 - p[1], &mut next);
                    cut(&next, |p| sign * (1.0 - (p[0] - fi) - (p[1] - fj)), &mut work);
                    if work.len() < 3 {
                        continue;
                    }
                    let h = corners.map(|c| self.h[c]);
                    let vertex = |p: &Point| {
                        let (u, v) = (p[0] - fi, p[1] - fj);
                        let y = if lower { h[0] + (h[1] - h[0]) * u + (h[2] - h[0]) * v } else { h[0] + (h[1] - h[0]) * (1.0 - u) + (h[2] - h[0]) * (1.0 - v) };
                        // Shaded like the ground it lies on.
                        let n = |di: usize, dj: usize, c: usize| self.normals[((j + dj) * row + i + di) * 3 + c] as f64;
                        let normal = [0, 1, 2].map(|c| (n(0, 0, c) * (1.0 - u) + n(1, 0, c) * u) * (1.0 - v) + (n(0, 1, c) * (1.0 - u) + n(1, 1, c) * u) * v);
                        let (x, z) = (self.x0 + p[0] * self.cell, self.z0 + p[1] * self.cell);
                        let mut y = y + LIFT;
                        // At a junction the road climbs off the verge to meet the route's tarmac, a touch proud of it.
                        if let Some((d, level)) = shaped.then(|| (self.route)((x, z))).flatten() {
                            let edge = ROAD_WIDTH / 2.0;
                            y += (level + ROAD_LIFT + 0.03 - y).max(0.0) * (1.0 - smoothstep(edge + 0.3, edge + RAMP, d));
                        }
                        ([x, y, z], normal, [p[2] as f32, p[3] as f32])
                    };
                    for k in 1..work.len() - 1 {
                        let (a, b, c) = (&work[0], &work[k], &work[k + 1]);
                        // Wound to face upward.
                        let turn = (b[1] - a[1]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[1] - a[1]);
                        if turn.abs() < 1e-9 {
                            continue;
                        }
                        for p in if turn > 0.0 { [a, b, c] } else { [a, c, b] } {
                            let (position, normal, uv) = vertex(p);
                            mesh.vertex(position, normal, colour, uv);
                        }
                    }
                }
            }
        }
    }
}
