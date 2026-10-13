//! Triangle soup shared by everything that isn't a height grid: buildings,
//! bridges, tunnels.

/// Three vertices per triangle, unindexed.
#[derive(Default)]
pub struct TriMesh {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub colors: Vec<f32>,
    /// Texture coordinates. Only buildings use them (into a repeating one-window
    /// tile: on walls, one unit per window bay across and per storey down from the eaves).
    pub uvs: Vec<f32>,
}

impl TriMesh {
    /// Adds a triangle facing the same way as `facing` (need not be exact or unit length).
    /// The stored normal is the triangle's true one, so sloped roofs shade correctly.
    pub(super) fn triangle(&mut self, mut p: [[f64; 3]; 3], facing: [f64; 3], mut colors: [[f32; 3]; 3], mut uvs: [[f32; 2]; 3]) {
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
            self.vertex(p[k], n.map(|v| v / len), colors[k], uvs[k]);
        }
    }

    /// Adds a four-sided face given its corners in order round the edge.
    pub(super) fn quad(&mut self, p: [[f64; 3]; 4], facing: [f64; 3], colors: [[f32; 3]; 4]) {
        self.triangle([p[0], p[1], p[2]], facing, [colors[0], colors[1], colors[2]], [[0.0; 2]; 3]);
        self.triangle([p[0], p[2], p[3]], facing, [colors[0], colors[2], colors[3]], [[0.0; 2]; 3]);
    }

    /// Adds one vertex as given, for surfaces that supply their own smooth normals.
    pub(super) fn vertex(&mut self, p: [f64; 3], normal: [f64; 3], color: [f32; 3], uv: [f32; 2]) {
        self.positions.extend(p.map(|v| v as f32));
        self.normals.extend(normal.map(|v| v as f32));
        self.colors.extend(color);
        self.uvs.extend(uv);
    }

    pub fn vertices(&self) -> usize {
        self.positions.len() / 3
    }
}
