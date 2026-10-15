//! The native renderer: draws the 3D world with the GPU directly (Metal, through wgpu)
//! instead of three.js inside the web page. The page stays on top, transparent, for the HUD.
//!
//! Everything it draws is already built in Rust (terrain, roads, buildings, water, trees,
//! bridges and tunnels) except a few things the page makes once and hands over at the start:
//! textures, text signs, the small props along the route and the rider in a few pedal positions.

mod host;

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use serde::Deserialize;
use tauri::{AppHandle, Manager};
use wgpu::util::DeviceExt;
use wintermiles_core::ride::{RideEngine, RivalKind};
use wintermiles_core::world::{GridMesh, World, LAYOUT};

use host::HostView;

const SAMPLES: u32 = 4;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Per-draw uniforms sit at this stride in one buffer.
const DRAW_STRIDE: u64 = 256;
const MAX_DRAWS: u64 = 64;
const MAX_IN_FLIGHT: usize = 6;
const FRAME: Duration = Duration::from_micros(16_000);

const CAMERAS: [[f64; 5]; 4] = [
    // back, up, side, look ahead, look up
    [7.5, 3.0, 0.0, 9.0, 1.3],
    [24.0, 13.0, 0.0, 6.0, 1.0],
    [-1.5, 2.0, 9.0, 0.0, 1.1],
    [38.0, 62.0, 0.0, 22.0, 0.0],
];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    camera: [f32; 4],
    sun_dir: [f32; 4],
    sun_color: [f32; 4],
    hemi_sky: [f32; 4],
    hemi_ground: [f32; 4],
    fog_color: [f32; 4],
    fog: [f32; 4],
    lamp: [f32; 4],
    sky_top: [f32; 4],
    sky_bottom: [f32; 4],
    cam_right: [f32; 4],
    cam_up: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Draw {
    model: [[f32; 4]; 4],
    tint: [f32; 4],
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LabelVertex {
    centre: [f32; 3],
    corner: [f32; 2],
    size: [f32; 2],
    uv: [f32; 2],
}

/// Time of day and weather, as the page works them out (linear colours, intensities applied).
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct Look {
    zenith: [f32; 3],
    horizon: [f32; 3],
    fog_color: [f32; 3],
    fog_near: f32,
    fog_far: f32,
    hemi_sky: [f32; 3],
    hemi_ground: [f32; 3],
    sun_color: [f32; 3],
    sun_dir: [f32; 3],
    lamp: f32,
}

impl Default for Look {
    fn default() -> Self {
        let srgb = |h: u32| [(h >> 16) & 255, (h >> 8) & 255, h & 255].map(|c| ((c as f32 / 255.0 + 0.055) / 1.055).powf(2.4));
        let scale = |c: [f32; 3], k: f32| c.map(|v| v * k);
        Look {
            zenith: srgb(0x3f86d6),
            horizon: srgb(0xbfd6e8),
            fog_color: srgb(0xbfd6e8),
            fog_near: 300.0,
            fog_far: 8500.0,
            hemi_sky: scale(srgb(0xe6f1ff), 2.0),
            hemi_ground: scale(srgb(0x6b7a4f), 2.0),
            sun_color: scale(srgb(0xfff1d6), 2.4),
            sun_dir: [0.4, 0.8, 0.28],
            lamp: 0.0,
        }
    }
}

/// What the page hands over at the start, described by a JSON header in front of the raw data.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetHeader {
    textures: Vec<TextureDef>,
    /// Vertex counts: props along the route, then each rider pose in turn.
    props: usize,
    poses: Vec<usize>,
    labels: Vec<LabelDef>,
    look: Look,
}

#[derive(Deserialize)]
struct TextureDef {
    name: String,
    w: u32,
    h: u32,
    /// "repeat", "clampS" (clamped across, repeating along) or "clamp".
    wrap: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LabelDef {
    /// World position of the centre, or for a rider's name tag the rider's id (placed above them).
    at: [f32; 3],
    rider: Option<String>,
    size: [f32; 2],
    uv: [f32; 4],
}

struct Assets {
    textures: Vec<(TextureDef, Vec<u8>)>,
    props: Vec<Vertex>,
    poses: Vec<Vec<Vertex>>,
    labels: Vec<LabelDef>,
    look: Look,
}

fn parse_assets(bytes: &[u8]) -> Result<Assets, String> {
    let len = u32::from_le_bytes(bytes.get(0..4).ok_or("empty")?.try_into().unwrap()) as usize;
    let header: AssetHeader = serde_json::from_slice(bytes.get(4..4 + len).ok_or("short header")?).map_err(|e| e.to_string())?;
    let mut at = (4 + len).next_multiple_of(4);
    let mut take = |n: usize| -> Result<&[u8], String> {
        let s = bytes.get(at..at + n).ok_or("asset data is shorter than its header says")?;
        at = (at + n).next_multiple_of(4);
        Ok(s)
    };
    let mut textures = Vec::new();
    for t in header.textures {
        let data = take((t.w * t.h * 4) as usize)?.to_vec();
        textures.push((t, data));
    }
    let verts = |b: &[u8]| -> Vec<Vertex> { bytemuck::pod_collect_to_vec(b) };
    let props = verts(take(header.props * std::mem::size_of::<Vertex>())?);
    let mut poses = Vec::new();
    for n in header.poses {
        poses.push(verts(take(n * std::mem::size_of::<Vertex>())?));
    }
    Ok(Assets { textures, props, poses, labels: header.labels, look: header.look })
}

/// Commands from the page while the renderer runs.
#[derive(Default)]
struct Control {
    camera: usize,
    look: Option<Look>,
    capture: Option<std::path::PathBuf>,
    frames: u64,
    size: (u32, u32),
    /// False while the window can't be seen; drawing pauses.
    hidden: bool,
    error: Option<String>,
}

pub struct Native {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    control: Arc<Mutex<Control>>,
    host: Arc<Mutex<Option<HostView>>>,
    app: AppHandle,
}

impl Native {
    pub fn start(app: &AppHandle, world: Arc<World>, engine: Arc<Mutex<RideEngine>>, assets: &[u8]) -> Result<Native, String> {
        let assets = parse_assets(assets).map_err(|e| format!("The page's drawing data could not be read: {e}"))?;
        let window = app.get_webview_window("main").ok_or("No main window.")?;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });

        // The view and the surface on it have to be made on the main thread.
        let (tx, rx) = mpsc::channel();
        let made_instance = instance.clone();
        let ns_window = window_pointer(&window)?;
        app.run_on_main_thread(move || {
            let made = (|| {
                let host = HostView::create(ns_window.get())?;
                let size = host.size();
                let (display, handle) = host.handles();
                // SAFETY: the view outlives the surface: it is removed only after the render thread ends.
                let surface = unsafe { made_instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle { raw_display_handle: display, raw_window_handle: handle }) }.map_err(|e| e.to_string())?;
                Ok::<_, String>((host, surface, size))
            })();
            let _ = tx.send(made);
        })
        .map_err(|e| e.to_string())?;
        let (host, surface, size) = rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "The window did not answer.".to_string())??;

        let control = Arc::new(Mutex::new(Control { size, ..Default::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        let host = Arc::new(Mutex::new(Some(host)));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::spawn({
            let (control, stop, app, host) = (control.clone(), stop.clone(), app.clone(), host.clone());
            move || {
                let renderer = pollster::block_on(Renderer::new(instance, surface, size, &assets, &world));
                match renderer {
                    Ok(mut r) => {
                        let _ = ready_tx.send(Ok(()));
                        r.run(world, engine, assets, control, stop, app, host);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            }
        });
        match ready_rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("The graphics card could not be set up: {e}")),
            Err(_) => return Err("Setting up the graphics card took too long.".into()),
        }
        Ok(Native { stop, thread: Some(thread), control, host, app: app.clone() })
    }

    pub fn cycle_camera(&self) {
        let mut c = self.control.lock().unwrap();
        c.camera = (c.camera + 1) % CAMERAS.len();
    }

    pub fn set_camera(&self, i: usize) {
        self.control.lock().unwrap().camera = i % CAMERAS.len();
    }

    pub fn set_look(&self, look: Look) {
        self.control.lock().unwrap().look = Some(look);
    }

    /// Asks for the next frame to be saved as a PNG; returns the frames drawn so far.
    pub fn capture(&self, path: std::path::PathBuf) -> u64 {
        let mut c = self.control.lock().unwrap();
        c.capture = Some(path);
        c.frames
    }

    pub fn frames(&self) -> (u64, Option<String>) {
        let c = self.control.lock().unwrap();
        (c.frames, c.error.clone())
    }

    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let host = self.host.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(h) = host.lock().unwrap().take() {
                h.remove();
            }
        });
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.halt();
    }
}

struct SendPtr(*mut std::ffi::c_void);
// SAFETY: only dereferenced on the main thread, in `HostView::create`.
unsafe impl Send for SendPtr {}

impl SendPtr {
    fn get(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

#[cfg(target_os = "macos")]
fn window_pointer(window: &tauri::WebviewWindow) -> Result<SendPtr, String> {
    window.ns_window().map(SendPtr).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn window_pointer(_: &tauri::WebviewWindow) -> Result<SendPtr, String> {
    Err("The native renderer is only available on macOS so far.".into())
}

// ── Maths ───────────────────────────────────────────────────────────────────

type Mat = [[f32; 4]; 4];

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [[0.0; 4]; 4];
    for (c, col) in r.iter_mut().enumerate() {
        for (row, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][row] * b[c][k]).sum();
        }
    }
    r
}

fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat {
    let f = 1.0 / (fov_y / 2.0).tan();
    // Depth 0 at the near plane, 1 at the far one.
    [[f / aspect, 0.0, 0.0, 0.0], [0.0, f, 0.0, 0.0], [0.0, 0.0, far / (near - far), -1.0], [0.0, 0.0, near * far / (near - far), 0.0]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-9);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// View matrix; positions are taken relative to `eye` already (camera-relative rendering keeps precision).
fn look_at(forward: [f64; 3]) -> (Mat, [f64; 3], [f64; 3]) {
    let f = norm(forward);
    let r = norm(cross(f, [0.0, 1.0, 0.0]));
    let u = cross(r, f);
    let m = [
        [r[0] as f32, u[0] as f32, -f[0] as f32, 0.0],
        [r[1] as f32, u[1] as f32, -f[1] as f32, 0.0],
        [r[2] as f32, u[2] as f32, -f[2] as f32, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    (m, r, u)
}

fn invert(m: &Mat) -> Mat {
    // General 4×4 inverse (cofactors), in f64 for accuracy.
    let a: Vec<f64> = (0..16).map(|i| m[i / 4][i % 4] as f64).collect();
    let mut inv = [0.0f64; 16];
    inv[0] = a[5] * a[10] * a[15] - a[5] * a[11] * a[14] - a[9] * a[6] * a[15] + a[9] * a[7] * a[14] + a[13] * a[6] * a[11] - a[13] * a[7] * a[10];
    inv[4] = -a[4] * a[10] * a[15] + a[4] * a[11] * a[14] + a[8] * a[6] * a[15] - a[8] * a[7] * a[14] - a[12] * a[6] * a[11] + a[12] * a[7] * a[10];
    inv[8] = a[4] * a[9] * a[15] - a[4] * a[11] * a[13] - a[8] * a[5] * a[15] + a[8] * a[7] * a[13] + a[12] * a[5] * a[11] - a[12] * a[7] * a[9];
    inv[12] = -a[4] * a[9] * a[14] + a[4] * a[10] * a[13] + a[8] * a[5] * a[14] - a[8] * a[6] * a[13] - a[12] * a[5] * a[10] + a[12] * a[6] * a[9];
    inv[1] = -a[1] * a[10] * a[15] + a[1] * a[11] * a[14] + a[9] * a[2] * a[15] - a[9] * a[3] * a[14] - a[13] * a[2] * a[11] + a[13] * a[3] * a[10];
    inv[5] = a[0] * a[10] * a[15] - a[0] * a[11] * a[14] - a[8] * a[2] * a[15] + a[8] * a[3] * a[14] + a[12] * a[2] * a[11] - a[12] * a[3] * a[10];
    inv[9] = -a[0] * a[9] * a[15] + a[0] * a[11] * a[13] + a[8] * a[1] * a[15] - a[8] * a[3] * a[13] - a[12] * a[1] * a[11] + a[12] * a[3] * a[9];
    inv[13] = a[0] * a[9] * a[14] - a[0] * a[10] * a[13] - a[8] * a[1] * a[14] + a[8] * a[2] * a[13] + a[12] * a[1] * a[10] - a[12] * a[2] * a[9];
    inv[2] = a[1] * a[6] * a[15] - a[1] * a[7] * a[14] - a[5] * a[2] * a[15] + a[5] * a[3] * a[14] + a[13] * a[2] * a[7] - a[13] * a[3] * a[6];
    inv[6] = -a[0] * a[6] * a[15] + a[0] * a[7] * a[14] + a[4] * a[2] * a[15] - a[4] * a[3] * a[14] - a[12] * a[2] * a[7] + a[12] * a[3] * a[6];
    inv[10] = a[0] * a[5] * a[15] - a[0] * a[7] * a[13] - a[4] * a[1] * a[15] + a[4] * a[3] * a[13] + a[12] * a[1] * a[7] - a[12] * a[3] * a[5];
    inv[14] = -a[0] * a[5] * a[14] + a[0] * a[6] * a[13] + a[4] * a[1] * a[14] - a[4] * a[2] * a[13] - a[12] * a[1] * a[6] + a[12] * a[2] * a[5];
    inv[3] = -a[1] * a[6] * a[11] + a[1] * a[7] * a[10] + a[5] * a[2] * a[11] - a[5] * a[3] * a[10] - a[9] * a[2] * a[7] + a[9] * a[3] * a[6];
    inv[7] = a[0] * a[6] * a[11] - a[0] * a[7] * a[10] - a[4] * a[2] * a[11] + a[4] * a[3] * a[10] + a[8] * a[2] * a[7] - a[8] * a[3] * a[6];
    inv[11] = -a[0] * a[5] * a[11] + a[0] * a[7] * a[9] + a[4] * a[1] * a[11] - a[4] * a[3] * a[9] - a[8] * a[1] * a[7] + a[8] * a[3] * a[5];
    inv[15] = a[0] * a[5] * a[10] - a[0] * a[6] * a[9] - a[4] * a[1] * a[10] + a[4] * a[2] * a[9] + a[8] * a[1] * a[6] - a[8] * a[2] * a[5];
    let det = a[0] * inv[0] + a[1] * inv[4] + a[2] * inv[8] + a[3] * inv[12];
    let k = if det.abs() > 1e-20 { 1.0 / det } else { 0.0 };
    let mut out = [[0.0f32; 4]; 4];
    for i in 0..16 {
        out[i / 4][i % 4] = (inv[i] * k) as f32;
    }
    out
}

/// Translation by `t`, then rotation about y by `yaw` and about x by `pitch` (three.js "YXZ").
fn model(t: [f32; 3], yaw: f32, pitch: f32) -> Mat {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    // Ry · Rx
    [[cy, 0.0, -sy, 0.0], [sy * sp, cp, cy * sp, 0.0], [sy * cp, -sp, cy * cp, 0.0], [t[0], t[1], t[2], 1.0]]
}


// ── GPU resources ───────────────────────────────────────────────────────────

struct Mesh {
    vertices: wgpu::Buffer,
    count: u32,
    indices: Option<(wgpu::Buffer, u32)>,
}

impl Mesh {
    fn new(device: &wgpu::Device, verts: &[Vertex], indices: Option<&[u32]>) -> Option<Mesh> {
        if verts.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(verts), usage: wgpu::BufferUsages::VERTEX });
        let indices = indices.filter(|i| !i.is_empty()).map(|i| (device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(i), usage: wgpu::BufferUsages::INDEX }), i.len() as u32));
        Some(Mesh { vertices, count: verts.len() as u32, indices })
    }

    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        match &self.indices {
            Some((b, n)) => {
                pass.set_index_buffer(b.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*n, 0, 0..1);
            }
            None => pass.draw(0..self.count, 0..1),
        }
    }
}

struct Chunk {
    ground: Mesh,
    trees: Option<(wgpu::Buffer, u32)>,
    buildings: Option<Mesh>,
    water: Option<Mesh>,
    roads: Option<Mesh>,
    /// Ground positions, for height look-ups.
    heights: Vec<f32>,
}

/// Vertex positions are uploaded relative to this, and redone when the rider gets far from it.
const REBASE_M: f64 = 4000.0;

fn grid_indices(cells: usize) -> Vec<u32> {
    let row = cells + 1;
    let mut idx = Vec::with_capacity(cells * cells * 6);
    for j in 0..cells {
        for i in 0..cells {
            let a = (j * row + i) as u32;
            let r = row as u32;
            idx.extend_from_slice(&[a, a + r, a + 1, a + 1, a + r, a + r + 1]);
        }
    }
    idx
}

fn verts_from(positions: &[f32], normals: Option<&[f32]>, colors: &[f32], uvs: Option<&[f32]>, origin: [f64; 2]) -> Vec<Vertex> {
    let n = positions.len() / 3;
    (0..n)
        .map(|i| Vertex {
            pos: [(positions[i * 3] as f64 - origin[0]) as f32, positions[i * 3 + 1], (positions[i * 3 + 2] as f64 - origin[1]) as f32],
            normal: normals.map_or([0.0, 1.0, 0.0], |nm| [nm[i * 3], nm[i * 3 + 1], nm[i * 3 + 2]]),
            color: [colors[i * 3], colors[i * 3 + 1], colors[i * 3 + 2]],
            uv: uvs.map_or([0.5, 0.5], |u| [u[i * 2], u[i * 2 + 1]]),
        })
        .collect()
}

/// Halves an RGBA image (box filter), for mipmaps.
fn half(src: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let px = |xx: u32, yy: u32| src[((yy.min(h - 1) * w + xx.min(w - 1)) * 4 + c) as usize] as u32;
                let s = px(2 * x, 2 * y) + px(2 * x + 1, 2 * y) + px(2 * x, 2 * y + 1) + px(2 * x + 1, 2 * y + 1);
                out[((y * nw + x) * 4 + c as u32) as usize] = (s / 4) as u8;
            }
        }
    }
    (out, nw, nh)
}

struct Pipelines {
    sky: wgpu::RenderPipeline,
    opaque: wgpu::RenderPipeline,
    overlay: wgpu::RenderPipeline,
    blended: wgpu::RenderPipeline,
    trees: wgpu::RenderPipeline,
    labels: wgpu::RenderPipeline,
}

struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipes: Pipelines,
    globals: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    draws: wgpu::Buffer,
    draws_bg: wgpu::BindGroup,
    /// By name: "white", "road", "windows", "ways", "labels".
    textures: HashMap<String, wgpu::BindGroup>,
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    tree_mesh: Mesh,
}

impl Renderer {
    async fn new(instance: wgpu::Instance, surface: wgpu::Surface<'static>, size: (u32, u32), assets: &Assets, _world: &World) -> Result<Renderer, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: Some(&surface), force_fallback_adapter: false })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor { label: Some("WinterMiles"), ..Default::default() }).await.map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.0,
            height: size.1,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("world"), source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });
        let uniform_layout = |dynamic: bool, size: u64| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: dynamic, min_binding_size: NonZeroU64::new(size) },
                    count: None,
                }],
            })
        };
        let globals_layout = uniform_layout(false, std::mem::size_of::<Globals>() as u64);
        let draw_layout = uniform_layout(true, std::mem::size_of::<Draw>() as u64);
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&globals_layout, &tex_layout, &draw_layout], push_constant_ranges: &[] });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x2],
        };
        let tree_instance = wgpu::VertexBufferLayout { array_stride: 24, step_mode: wgpu::VertexStepMode::Instance, attributes: &wgpu::vertex_attr_array![4 => Float32x3, 5 => Float32x3] };
        let label_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LabelVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2],
        };
        let pipeline = |vs: &str, fs: &str, buffers: &[wgpu::VertexBufferLayout], blend: bool, depth_write: bool, compare: wgpu::CompareFunction, bias: (i32, f32)| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some(vs), buffers, compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: Some(if blend { wgpu::BlendState::ALPHA_BLENDING } else { wgpu::BlendState::REPLACE }), write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: depth_write,
                    depth_compare: compare,
                    stencil: Default::default(),
                    bias: wgpu::DepthBiasState { constant: bias.0, slope_scale: bias.1, clamp: 0.0 },
                }),
                multisample: wgpu::MultisampleState { count: SAMPLES, mask: !0, alpha_to_coverage_enabled: false },
                multiview: None,
                cache: None,
            })
        };
        use wgpu::CompareFunction::*;
        let pipes = Pipelines {
            sky: pipeline("vs_sky", "fs_sky", &[], false, false, Always, (0, 0.0)),
            opaque: pipeline("vs_main", "fs_main", &[vertex_layout.clone()], false, true, Less, (0, 0.0)),
            overlay: pipeline("vs_main", "fs_main", &[vertex_layout.clone()], false, false, LessEqual, (-4, -2.0)),
            blended: pipeline("vs_main", "fs_main", &[vertex_layout.clone()], true, false, Less, (0, 0.0)),
            trees: pipeline("vs_tree", "fs_main", &[vertex_layout.clone(), tree_instance], false, true, Less, (0, 0.0)),
            labels: pipeline("vs_label", "fs_label", &[label_layout], true, false, Less, (0, 0.0)),
        };

        let globals = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: std::mem::size_of::<Globals>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &globals_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }] });
        let draws = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: DRAW_STRIDE * MAX_DRAWS, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let draws_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &draw_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &draws, offset: 0, size: NonZeroU64::new(std::mem::size_of::<Draw>() as u64) }) }],
        });

        let mut textures = HashMap::new();
        let mut add_texture = |name: &str, w: u32, h: u32, rgba: &[u8], wrap: &str| {
            let levels = 32 - w.max(h).leading_zeros();
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(name),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: levels,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let (mut data, mut lw, mut lh) = (rgba.to_vec(), w, h);
            for level in 0..levels {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    &data,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * lw), rows_per_image: Some(lh) },
                    wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
                );
                (data, lw, lh) = half(&data, lw, lh);
            }
            let mode = |repeat: bool| if repeat { wgpu::AddressMode::Repeat } else { wgpu::AddressMode::ClampToEdge };
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: mode(wrap == "repeat"),
                address_mode_v: mode(wrap == "repeat" || wrap == "clampS"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                anisotropy_clamp: 8,
                ..Default::default()
            });
            let view = texture.create_view(&Default::default());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &tex_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) }],
            });
            textures.insert(name.to_string(), bg);
        };
        add_texture("white", 1, 1, &[255, 255, 255, 255], "repeat");
        for (t, data) in &assets.textures {
            add_texture(&t.name, t.w, t.h, data, &t.wrap);
        }
        let (msaa, depth) = targets(&device, &config);
        let tree_mesh = Mesh::new(&device, &tree_geometry(), None).unwrap();
        Ok(Renderer { device, queue, surface, config, pipes, globals, globals_bg, draws, draws_bg, textures, msaa, depth, tree_mesh })
    }

    fn resize(&mut self, size: (u32, u32)) {
        if size == (self.config.width, self.config.height) || size.0 == 0 || size.1 == 0 {
            return;
        }
        self.config.width = size.0;
        self.config.height = size.1;
        self.surface.configure(&self.device, &self.config);
        (self.msaa, self.depth) = targets(&self.device, &self.config);
    }

    #[allow(clippy::too_many_arguments)]
    fn run(&mut self, world: Arc<World>, engine: Arc<Mutex<RideEngine>>, assets: Assets, control: Arc<Mutex<Control>>, stop: Arc<AtomicBool>, app: AppHandle, host: Arc<Mutex<Option<HostView>>>) {
        let route = world.route.clone();
        let mut look = assets.look;
        let mut origin = [route.x[0], route.z[0]];
        let mut scene = SceneMeshes::build(&self.device, &world, &assets, origin);
        let mut chunks: HashMap<(i32, i32), Chunk> = HashMap::new();
        let mut far: Option<(Mesh, (i64, i64))> = None;
        let (job_tx, job_rx) = mpsc::channel::<(i32, i32)>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (done_tx, done_rx) = mpsc::channel::<((i32, i32), GridMesh)>();
        // Two workers build terrain chunks while frames keep coming.
        for _ in 0..2 {
            let (rx, tx, world, stop) = (job_rx.clone(), done_tx.clone(), world.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Ok(job) = rx.lock().unwrap().recv_timeout(Duration::from_millis(200)) else { continue };
                    if tx.send((job, world.chunk(job.0, job.1))).is_err() {
                        break;
                    }
                }
            });
        }
        let (far_tx, far_rx) = mpsc::channel::<((i64, i64), GridMesh)>();
        let mut far_busy = false;
        let mut pending: HashSet<(i32, i32)> = HashSet::new();
        let grid_idx = grid_indices(LAYOUT.cells);
        let far_idx = grid_indices(LAYOUT.far_cells);

        let mut last = Instant::now();
        let mut shown = engine.lock().unwrap().distance;
        let mut cam: Option<([f64; 3], [f64; 3], f64)> = None;
        let mut crank = 0.0f64;
        let mut size_check = Instant::now();
        let mut lanes: HashMap<String, f64> = HashMap::new();

        while !stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            let dt = (now - last).as_secs_f64().min(0.1);
            last = now;

            // Keep the window size current (asked of the main thread now and then).
            if now - size_check > Duration::from_millis(400) {
                size_check = now;
                let (h, c) = (host.clone(), control.clone());
                let _ = app.run_on_main_thread(move || {
                    if let Some(v) = h.lock().unwrap().as_ref() {
                        let mut c = c.lock().unwrap();
                        c.size = v.size();
                        c.hidden = !v.visible();
                    }
                });
            }
            let (size, cam_mode, new_look, capture, hidden) = {
                let mut c = control.lock().unwrap();
                let capture = c.capture.take();
                let hidden = c.hidden && capture.is_none();
                (c.size, c.camera, c.look.take(), capture, hidden)
            };
            // Nobody can see it: don't spend the power (a pending capture still gets its frame).
            if hidden {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            self.resize(size);
            if let Some(l) = new_look {
                look = l;
            }

            // Where everyone is. The simulation steps at 50 Hz; glide between its steps.
            let (distance, speed, cadence, rivals) = {
                let e = engine.lock().unwrap();
                let rivals: Vec<(String, RivalKind, u32, f64, f64)> = e.rivals.iter().map(|r| (r.id.clone(), r.kind, r.color, r.distance, r.speed)).collect();
                (e.distance, if e.moving { e.speed } else { 0.0 }, e.cadence.unwrap_or(if e.moving && e.power > 20.0 { 80.0 } else { 0.0 }), rivals)
            };
            shown += speed * dt;
            shown += (distance - shown) * (1.0 - (-dt * 6.0).exp());
            if (distance - shown).abs() > 30.0 {
                shown = distance;
            }
            crank += cadence / 60.0 * dt;
            let p = sample(&route, shown);

            // Re-centre the coordinates if the rider has gone far from the origin.
            if (p.x - origin[0]).abs() > REBASE_M || (p.z - origin[1]).abs() > REBASE_M {
                origin = [p.x, p.z];
                scene = SceneMeshes::build(&self.device, &world, &assets, origin);
                chunks.clear();
                pending.clear();
                far = None;
            }

            // Terrain streaming: what's missing, nearest first; drop what's far behind.
            let (ccx, ccz) = ((p.x / LAYOUT.chunk).floor() as i32, (p.z / LAYOUT.chunk).floor() as i32);
            let view = LAYOUT.view_chunks;
            'rings: for ring in 0..=view {
                for dx in -ring..=ring {
                    for dz in -ring..=ring {
                        if dx.abs().max(dz.abs()) != ring || (dx * dx + dz * dz) as f64 > (view as f64 + 0.5).powi(2) {
                            continue;
                        }
                        if pending.len() >= MAX_IN_FLIGHT {
                            break 'rings;
                        }
                        let k = (ccx + dx, ccz + dz);
                        if !chunks.contains_key(&k) && pending.insert(k) {
                            let _ = job_tx.send(k);
                        }
                    }
                }
            }
            chunks.retain(|k, _| (k.0 - ccx).abs() <= view + 2 && (k.1 - ccz).abs() <= view + 2);
            for _ in 0..3 {
                let Ok((k, mesh)) = done_rx.try_recv() else { break };
                pending.remove(&k);
                if (k.0 - ccx).abs() <= view + 2 && (k.1 - ccz).abs() <= view + 2 {
                    if let Some(c) = self.upload_chunk(&mesh, &grid_idx, origin) {
                        chunks.insert(k, c);
                    }
                }
            }
            let far_key = ((p.x / LAYOUT.far_cell).round() as i64, (p.z / LAYOUT.far_cell).round() as i64);
            if !far_busy && far.as_ref().is_none_or(|f| f.1 != far_key) {
                far_busy = true;
                let (w, tx) = (world.clone(), far_tx.clone());
                std::thread::spawn(move || {
                    let mesh = w.far(far_key.0 as f64 * LAYOUT.far_cell, far_key.1 as f64 * LAYOUT.far_cell);
                    let _ = tx.send((far_key, mesh));
                });
            }
            if let Ok((k, mesh)) = far_rx.try_recv() {
                far_busy = false;
                let v = verts_from(&mesh.positions, Some(&mesh.normals), &mesh.colors, None, origin);
                far = Mesh::new(&self.device, &v, Some(&far_idx)).map(|m| (m, k));
            }

            // Camera, as the page's chase camera does it.
            let c = CAMERAS[cam_mode];
            let (eye, target) = {
                let heading = match cam {
                    Some((_, _, h)) => {
                        let dh = (p.heading - h).sin().atan2((p.heading - h).cos());
                        h + dh * (1.0 - (-dt * 2.2).exp())
                    }
                    None => p.heading,
                };
                let (fx, fz) = (heading.sin(), heading.cos());
                let mut want = [p.x - fx * c[0] + fz * c[2], p.ele + c[1], p.z - fz * c[0] - fx * c[2]];
                if let Some(h) = height_at(&chunks, want[0], want[2]) {
                    want[1] = want[1].max(h + 1.4);
                }
                let look_at = [p.x + fx * c[3], p.ele + c[4] + p.grade * c[3], p.z + fz * c[3]];
                let (eye, target) = match cam {
                    Some((e, t, _)) => {
                        let k = 1.0 - (-dt * 4.0).exp();
                        (std::array::from_fn(|i| e[i] + (want[i] - e[i]) * k), std::array::from_fn(|i| t[i] + (look_at[i] - t[i]) * k))
                    }
                    None => (want, look_at),
                };
                cam = Some((eye, target, heading));
                (eye, target)
            };

            // Uniforms. Everything is drawn relative to the camera (for precision far from the start).
            let rel = |q: [f64; 3]| [(q[0] - eye[0]) as f32, (q[1] - eye[1]) as f32, (q[2] - eye[2]) as f32];
            let (view_m, right, up) = look_at(sub(target, eye));
            let aspect = self.config.width as f32 / self.config.height as f32;
            let proj = perspective(58f32.to_radians(), aspect, 0.5, 20000.0);
            let view_proj = mul(&proj, &view_m);
            let sun = norm([look.sun_dir[0] as f64, look.sun_dir[1] as f64, look.sun_dir[2] as f64]);
            let lamp_at = rel([p.x + p.heading.sin() * 6.0, p.ele + 3.0, p.z + p.heading.cos() * 6.0]);
            let g = Globals {
                view_proj,
                inv_view_proj: invert(&view_proj),
                camera: [0.0, 0.0, 0.0, 1.0],
                sun_dir: [sun[0] as f32, sun[1] as f32, sun[2] as f32, 0.0],
                sun_color: [look.sun_color[0], look.sun_color[1], look.sun_color[2], 0.0],
                hemi_sky: [look.hemi_sky[0], look.hemi_sky[1], look.hemi_sky[2], 0.0],
                hemi_ground: [look.hemi_ground[0], look.hemi_ground[1], look.hemi_ground[2], 0.0],
                fog_color: [look.fog_color[0], look.fog_color[1], look.fog_color[2], 1.0],
                fog: [look.fog_near, look.fog_far, 0.0, 0.0],
                lamp: [lamp_at[0], lamp_at[1], lamp_at[2], look.lamp],
                sky_top: [look.zenith[0], look.zenith[1], look.zenith[2], 1.0],
                sky_bottom: [look.horizon[0], look.horizon[1], look.horizon[2], 1.0],
                cam_right: [right[0] as f32, right[1] as f32, right[2] as f32, 0.0],
                cam_up: [up[0] as f32, up[1] as f32, up[2] as f32, 0.0],
            };
            self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));

            // Per-draw slots: 0 the world (shifted from its origin to the camera), 1 the same unlit,
            // 2 water, then one per rider.
            let world_shift = [(origin[0] - eye[0]) as f32, -eye[1] as f32, (origin[1] - eye[2]) as f32];
            let shifted = model(world_shift, 0.0, 0.0);
            let mut slots: Vec<Draw> = vec![
                Draw { model: shifted, tint: [1.0; 4], flags: [1.0, 0.0, 0.0, 0.0] },
                Draw { model: shifted, tint: [1.0; 4], flags: [0.0, 0.0, 0.0, 0.0] },
                Draw { model: shifted, tint: [1.0, 1.0, 1.0, 0.86], flags: [1.0, 0.0, 0.0, 0.0] },
                Draw { model: shifted, tint: [1.0; 4], flags: [1.0, 0.0, 1.0, 0.0] },
            ];
            let mut riders: Vec<(usize, usize, bool)> = Vec::new();
            let pose_count = assets.poses.len().max(1);
            let pose = ((crank.fract() * pose_count as f64) as usize) % pose_count;
            let place = |q: &Pos, lane: f64| -> Mat {
                let at = rel([q.x + q.heading.cos() * lane, q.ele + LAYOUT.road_lift, q.z - q.heading.sin() * lane]);
                model(at, q.heading as f32, -(q.grade.atan()) as f32)
            };
            // The rider's own colour, #ff5a36, in linear light.
            slots.push(Draw { model: place(&p, 0.0), tint: [1.0, 0.102, 0.036, 1.0], flags: [1.0, 1.0, 0.0, 0.0] });
            riders.push((slots.len() - 1, pose, false));
            let mut rider_tags: Vec<(String, [f64; 3])> = Vec::new();
            for (i, (id, kind, color, d, _)) in rivals.iter().enumerate() {
                let gap = (d - shown).abs();
                if gap > 700.0 || *d <= 0.0 || *d >= route.total || slots.len() as u64 >= MAX_DRAWS {
                    continue;
                }
                let lane = *lanes.entry(id.clone()).or_insert_with(|| (if i % 2 == 1 { 1.0 } else { -1.0 }) * (1.3 + 0.5 * (i / 2) as f64));
                let q = sample(&route, *d);
                let ghost = *kind == RivalKind::Ghost;
                let rgb = [((color >> 16) & 255) as f32 / 255.0, ((color >> 8) & 255) as f32 / 255.0, (color & 255) as f32 / 255.0].map(|c| c.powf(2.2));
                slots.push(Draw { model: place(&q, lane), tint: if ghost { [1.0, 1.0, 1.0, 0.38] } else { [rgb[0], rgb[1], rgb[2], 1.0] }, flags: [1.0, 1.0, 0.0, if ghost { 1.0 } else { 0.0 }] });
                riders.push((slots.len() - 1, (pose + i * 2) % pose_count, ghost));
                if gap < 250.0 && gap > 12.0 {
                    rider_tags.push((id.clone(), [q.x + q.heading.cos() * lane, q.ele + 2.3, q.z - q.heading.sin() * lane]));
                }
            }
            for (i, s) in slots.iter().enumerate() {
                self.queue.write_buffer(&self.draws, i as u64 * DRAW_STRIDE, bytemuck::bytes_of(s));
            }

            // Text signs: fixed ones plus name tags over the riders near by.
            let mut quads: Vec<LabelVertex> = Vec::with_capacity(assets.labels.len() * 6);
            for l in &assets.labels {
                let centre = match &l.rider {
                    Some(id) => match rider_tags.iter().find(|t| &t.0 == id) {
                        Some((_, at)) => {
                            let r = rel(*at);
                            let dist = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
                            let s = (dist / 55.0).clamp(0.3, 3.0);
                            push_quad(&mut quads, r, [s * l.size[0] / l.size[1], s], l.uv);
                            continue;
                        }
                        None => continue,
                    },
                    None => rel([l.at[0] as f64, l.at[1] as f64, l.at[2] as f64]),
                };
                if centre[0].abs() > 3000.0 || centre[2].abs() > 3000.0 {
                    continue;
                }
                push_quad(&mut quads, centre, l.size, l.uv);
            }
            let label_buf = (!quads.is_empty()).then(|| self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&quads), usage: wgpu::BufferUsages::VERTEX }));

            let frame = match self.surface.get_current_texture() {
                Ok(f) => f,
                Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                    self.surface.configure(&self.device, &self.config);
                    continue;
                }
                Err(e) => {
                    control.lock().unwrap().error = Some(e.to_string());
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            let target_view = frame.texture.create_view(&Default::default());
            let mut encoder = self.device.create_command_encoder(&Default::default());
            let capture_tex = capture.as_ref().map(|_| {
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d { width: self.config.width, height: self.config.height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.config.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            });
            let capture_view = capture_tex.as_ref().map(|t| t.create_view(&Default::default()));
            for resolve in std::iter::once(&target_view).chain(capture_view.as_ref()) {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.msaa,
                        resolve_target: Some(resolve),
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.5, g: 0.6, b: 0.7, a: 1.0 }), store: wgpu::StoreOp::Discard },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.depth,
                        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                let white = &self.textures["white"];
                let tex = |name: &str| self.textures.get(name).unwrap_or(white);
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_bind_group(1, white, &[]);
                pass.set_bind_group(2, &self.draws_bg, &[0]);
                pass.set_pipeline(&self.pipes.sky);
                pass.draw(0..3, 0..1);

                // Opaque world.
                pass.set_pipeline(&self.pipes.opaque);
                if let Some((m, _)) = &far {
                    m.draw(&mut pass);
                }
                for c in chunks.values() {
                    c.ground.draw(&mut pass);
                }
                pass.set_bind_group(1, tex("windows"), &[]);
                for c in chunks.values() {
                    if let Some(b) = &c.buildings {
                        b.draw(&mut pass);
                    }
                }
                pass.set_bind_group(1, tex("road"), &[]);
                if let Some(r) = &scene.route_road {
                    r.draw(&mut pass);
                }
                pass.set_bind_group(1, white, &[]);
                if let Some(m) = &scene.structures_lit {
                    m.draw(&mut pass);
                }
                if let Some(m) = &scene.props {
                    m.draw(&mut pass);
                }
                pass.set_bind_group(2, &self.draws_bg, &[DRAW_STRIDE as u32]);
                if let Some(m) = &scene.structures_unlit {
                    m.draw(&mut pass);
                }
                // Trees, flat-shaded.
                pass.set_pipeline(&self.pipes.trees);
                pass.set_bind_group(2, &self.draws_bg, &[3 * DRAW_STRIDE as u32]);
                pass.set_vertex_buffer(0, self.tree_mesh.vertices.slice(..));
                for c in chunks.values() {
                    if let Some((buf, n)) = &c.trees {
                        pass.set_vertex_buffer(1, buf.slice(..));
                        pass.draw(0..self.tree_mesh.count, 0..*n);
                    }
                }
                // Riders.
                pass.set_pipeline(&self.pipes.opaque);
                for &(slot, pose, ghost) in &riders {
                    if ghost {
                        continue;
                    }
                    if let Some(m) = scene.poses.get(pose) {
                        pass.set_bind_group(2, &self.draws_bg, &[(slot as u64 * DRAW_STRIDE) as u32]);
                        m.draw(&mut pass);
                    }
                }
                // Roads around the route, laid on the ground.
                pass.set_pipeline(&self.pipes.overlay);
                pass.set_bind_group(1, tex("ways"), &[]);
                pass.set_bind_group(2, &self.draws_bg, &[0]);
                for c in chunks.values() {
                    if let Some(r) = &c.roads {
                        r.draw(&mut pass);
                    }
                }
                // Water and ghosts, see-through.
                pass.set_pipeline(&self.pipes.blended);
                pass.set_bind_group(1, white, &[]);
                pass.set_bind_group(2, &self.draws_bg, &[2 * DRAW_STRIDE as u32]);
                for c in chunks.values() {
                    if let Some(w) = &c.water {
                        w.draw(&mut pass);
                    }
                }
                for &(slot, pose, ghost) in &riders {
                    if !ghost {
                        continue;
                    }
                    if let Some(m) = scene.poses.get(pose) {
                        pass.set_bind_group(2, &self.draws_bg, &[(slot as u64 * DRAW_STRIDE) as u32]);
                        m.draw(&mut pass);
                    }
                }
                // Signs.
                if let Some(buf) = &label_buf {
                    pass.set_pipeline(&self.pipes.labels);
                    pass.set_bind_group(1, tex("labels"), &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..quads.len() as u32, 0..1);
                }
            }
            let readback = capture_tex.as_ref().map(|t| {
                let (w, h) = (self.config.width, self.config.height);
                let row = (w * 4).next_multiple_of(256);
                let buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
                (buf, w, h, row)
            });
            self.queue.submit([encoder.finish()]);
            frame.present();
            control.lock().unwrap().frames += 1;
            // Sixty frames a second is plenty for this and halves the work on 120 Hz displays.
            let spent = now.elapsed();
            if spent < FRAME {
                std::thread::sleep(FRAME - spent);
            }

            if let (Some(path), Some((buf, w, h, row))) = (capture, readback) {
                let bgra = matches!(self.config.format, wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm);
                let slice = buf.slice(..);
                let (tx, rx) = mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |r| {
                    let _ = tx.send(r);
                });
                let _ = self.device.poll(wgpu::PollType::Wait);
                if let Ok(Ok(())) = rx.recv() {
                    let data = slice.get_mapped_range();
                    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                    for y in 0..h {
                        for x in 0..w {
                            let i = (y * row + x * 4) as usize;
                            let px = &data[i..i + 4];
                            if bgra {
                                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                            } else {
                                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                            }
                        }
                    }
                    let _ = write_png(&path, w, h, &rgba);
                }
            }
        }
    }

    fn upload_chunk(&self, mesh: &GridMesh, grid_idx: &[u32], origin: [f64; 2]) -> Option<Chunk> {
        let ground = Mesh::new(&self.device, &verts_from(&mesh.positions, Some(&mesh.normals), &mesh.colors, None, origin), Some(grid_idx))?;
        let trees = (!mesh.trees.is_empty()).then(|| {
            let shifted: Vec<f32> = mesh.trees.chunks_exact(6).flat_map(|t| [(t[0] as f64 - origin[0]) as f32, t[1], (t[2] as f64 - origin[1]) as f32, t[3], t[4], t[5]]).collect();
            (self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&shifted), usage: wgpu::BufferUsages::VERTEX }), (shifted.len() / 6) as u32)
        });
        let b = &mesh.buildings;
        let buildings = Mesh::new(&self.device, &verts_from(&b.positions, Some(&b.normals), &b.colors, Some(&b.uvs), origin), None);
        let water = Mesh::new(&self.device, &verts_from(&mesh.water, None, &mesh.water_colors, None, origin), None);
        let r = &mesh.roads;
        let roads = Mesh::new(&self.device, &verts_from(&r.positions, Some(&r.normals), &r.colors, Some(&r.uvs), origin), None);
        Some(Chunk { ground, trees, buildings, water, roads, heights: mesh.positions.clone() })
    }
}

fn push_quad(out: &mut Vec<LabelVertex>, centre: [f32; 3], size: [f32; 2], uv: [f32; 4]) {
    let corner = |x: f32, y: f32, u: f32, v: f32| LabelVertex { centre, corner: [x, y], size, uv: [u, v] };
    let (a, b, c, d) = (corner(-1.0, -1.0, uv[0], uv[3]), corner(1.0, -1.0, uv[2], uv[3]), corner(1.0, 1.0, uv[2], uv[1]), corner(-1.0, 1.0, uv[0], uv[1]));
    out.extend_from_slice(&[a, b, c, a, c, d]);
}

fn targets(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> (wgpu::TextureView, wgpu::TextureView) {
    let make = |format: wgpu::TextureFormat| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: SAMPLES,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    (make(config.format), make(DEPTH))
}

/// The shared tree: a trunk and two cones, as the page's own tree is built.
fn tree_geometry() -> Vec<Vertex> {
    let mut out = Vec::new();
    let mut cone = |r0: f64, r1: f64, h: f64, y0: f64, sides: usize, color: [f32; 3]| {
        for s in 0..sides {
            let (a0, a1) = (s as f64 / sides as f64 * std::f64::consts::TAU, (s + 1) as f64 / sides as f64 * std::f64::consts::TAU);
            let p = |a: f64, r: f64, y: f64| [(a.sin() * r) as f32, y as f32, (a.cos() * r) as f32];
            let (b0, b1, t0, t1) = (p(a0, r0, y0), p(a1, r0, y0), p(a0, r1, y0 + h), p(a1, r1, y0 + h));
            for q in [b0, b1, t1, b0, t1, t0] {
                out.push(Vertex { pos: q, normal: [0.0, 1.0, 0.0], color, uv: [0.5, 0.5] });
            }
            // The base.
            for q in [[0.0, y0 as f32, 0.0], b1, b0] {
                out.push(Vertex { pos: q, normal: [0.0, -1.0, 0.0], color, uv: [0.5, 0.5] });
            }
        }
    };
    cone(0.3, 0.22, 1.8, 0.0, 5, [0.32, 0.22, 0.14]);
    cone(2.1, 0.0, 4.2, 1.5, 7, [0.13, 0.36, 0.17]);
    cone(1.5, 0.0, 3.6, 4.2, 7, [0.16, 0.42, 0.2]);
    out
}

/// Meshes that span the route rather than one chunk: the road, bridges and tunnels, props and riders.
struct SceneMeshes {
    route_road: Option<Mesh>,
    structures_lit: Option<Mesh>,
    structures_unlit: Option<Mesh>,
    props: Option<Mesh>,
    poses: Vec<Mesh>,
}

impl SceneMeshes {
    fn build(device: &wgpu::Device, world: &World, assets: &Assets, origin: [f64; 2]) -> SceneMeshes {
        let route = &world.route;
        let road = world.road();
        let section = LAYOUT.road_section;
        let n = route.n;
        // Daylight doesn't follow the road into a tunnel.
        let covered = &world.scene.covered;
        let dim = |i: usize| {
            let d = i as f64 * route.step;
            let deepest = covered.iter().map(|c| (d - c[0]).min(c[1] - d)).fold(0.0f64, f64::max);
            1.0 - 0.62 * (deepest / 12.0).clamp(0.0, 1.0) as f32
        };
        let mut road_verts = Vec::with_capacity(n * section);
        for i in 0..n * section {
            let c = dim(i / section);
            road_verts.push(Vertex {
                pos: [(road.positions[i * 3] as f64 - origin[0]) as f32, road.positions[i * 3 + 1], (road.positions[i * 3 + 2] as f64 - origin[1]) as f32],
                normal: [0.0, 1.0, 0.0],
                color: [c, c, c],
                uv: [road.uvs[i * 2], road.uvs[i * 2 + 1]],
            });
        }
        let mut idx = Vec::with_capacity((n - 1) * (section - 1) * 6);
        for i in 0..n - 1 {
            for k in 0..section - 1 {
                let a = (i * section + k) as u32;
                let b = a + section as u32;
                idx.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        let s = world.structures();
        let shift = |v: &[Vertex]| -> Vec<Vertex> { v.iter().map(|q| Vertex { pos: [(q.pos[0] as f64 - origin[0]) as f32, q.pos[1], (q.pos[2] as f64 - origin[1]) as f32], ..*q }).collect() };
        SceneMeshes {
            route_road: Mesh::new(device, &road_verts, Some(&idx)),
            structures_lit: Mesh::new(device, &verts_from(&s.lit.positions, Some(&s.lit.normals), &s.lit.colors, None, origin), None),
            structures_unlit: Mesh::new(device, &verts_from(&s.unlit.positions, None, &s.unlit.colors, None, origin), None),
            props: Mesh::new(device, &shift(&assets.props), None),
            // Rider poses are in the rider's own frame and placed by their draw's model matrix.
            poses: assets.poses.iter().filter_map(|p| Mesh::new(device, p, None)).collect(),
        }
    }
}

struct Pos {
    x: f64,
    z: f64,
    ele: f64,
    grade: f64,
    heading: f64,
}

/// A point along the route, as the page's `sampleAt` gives it.
fn sample(route: &wintermiles_core::gpx::Route, dist: f64) -> Pos {
    let f = (dist / route.step).clamp(0.0, (route.n - 1) as f64);
    let i = (f.floor() as usize).min(route.n - 2);
    let t = f - i as f64;
    let lerp = |a: f64, b: f64| a + (b - a) * t;
    let (a, b) = (i.saturating_sub(2), (i + 3).min(route.n - 1));
    Pos {
        x: lerp(route.x[i], route.x[i + 1]),
        z: lerp(route.z[i], route.z[i + 1]),
        ele: lerp(route.ele[i] as f64, route.ele[i + 1] as f64),
        grade: lerp(route.grade[i] as f64, route.grade[i + 1] as f64),
        heading: (route.x[b] - route.x[a]).atan2(route.z[b] - route.z[a]),
    }
}

/// Ground height from a loaded chunk, if the chunk under the point is in.
fn height_at(chunks: &HashMap<(i32, i32), Chunk>, x: f64, z: f64) -> Option<f64> {
    let (chunk, cells) = (LAYOUT.chunk, LAYOUT.cells);
    let (cx, cz) = ((x / chunk).floor() as i32, (z / chunk).floor() as i32);
    let c = chunks.get(&(cx, cz))?;
    let fx = (x / chunk - cx as f64) * cells as f64;
    let fz = (z / chunk - cz as f64) * cells as f64;
    let (i, j) = ((fx.floor() as usize).min(cells - 1), (fz.floor() as usize).min(cells - 1));
    let (u, v) = (fx - i as f64, fz - j as f64);
    let h = |ii: usize, jj: usize| c.heights[(jj * (cells + 1) + ii) * 3 + 1] as f64;
    Some((h(i, j) * (1.0 - u) + h(i + 1, j) * u) * (1.0 - v) + (h(i, j + 1) * (1.0 - u) + h(i + 1, j + 1) * u) * v)
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(rgba)).map_err(|e| e.to_string())
}
