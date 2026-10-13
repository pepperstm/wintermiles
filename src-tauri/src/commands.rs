//! The commands the UI can invoke. Each is a thin adapter onto the modules that do the work.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use tauri::ipc::Response;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use wintermiles_core::gamify::{Badge, BADGES};
use wintermiles_core::gpx::{build_route, demo_route, parse_gpx, Route};
use wintermiles_core::types::{HealthSummary, RideSummary, RouteMeta, Settings};
use wintermiles_core::world::GridMesh;

use crate::devices::DevicesSnapshot;
use crate::session::{RideInit, Session, DEMO_ROUTE_ID};
use crate::strava::{self, StravaStatus};
use crate::{health, now_ms, AppState};

type Res<T> = Result<T, String>;

fn blocking_err(e: tauri::Error) -> String {
    e.to_string()
}

// ── Storage ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn kv_get(state: State<AppState>, name: String) -> Option<Value> {
    state.store.get(&name)
}

#[tauri::command]
pub fn kv_set(state: State<AppState>, name: String, value: Value) -> Res<()> {
    state.store.set(&name, &value)
}

#[tauri::command]
pub fn kv_del(state: State<AppState>, name: String) -> Res<()> {
    state.store.del(&name)
}

// ── Routes ─────────────────────────────────────────────────────────────────

fn meta_for(id: String, route: &Route, added_at: u64) -> RouteMeta {
    RouteMeta { id, name: route.name.clone(), distance: route.total, ascent: route.ascent, climbs: route.climbs.len(), added_at, preview: route.preview_outline(160) }
}

/// Validates a GPX file and adds it to the library.
fn import_gpx(state: &AppState, file_name: &str, text: &str) -> Res<RouteMeta> {
    let (name, points) = parse_gpx(text)?;
    let name = if name.is_empty() { file_name.to_string() } else { name };
    let route = build_route(&name, &points)?;
    let meta = meta_for(uuid::Uuid::new_v4().to_string(), &route, now_ms());
    state.store.set(&format!("route-{}", meta.id), &serde_json::json!({ "name": name, "gpx": text }))?;
    Ok(meta)
}

#[derive(Serialize)]
pub struct ImportResult {
    added: Vec<RouteMeta>,
    problems: Vec<String>,
}

#[tauri::command]
pub async fn routes_import_dialog(app: AppHandle, state: State<'_, AppState>) -> Res<ImportResult> {
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().set_title("Import GPX routes").add_filter("GPX routes", &["gpx"]).blocking_pick_files())
        .await
        .map_err(blocking_err)?;
    let mut result = ImportResult { added: Vec::new(), problems: Vec::new() };
    for file in picked.unwrap_or_default() {
        let Ok(path) = file.into_path() else { continue };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Route").to_string();
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| import_gpx(&state, &stem, &text)) {
            Ok(meta) => result.added.push(meta),
            Err(e) => result.problems.push(format!("{stem}: {e}")),
        }
    }
    Ok(result)
}

#[tauri::command]
pub fn routes_import_text(state: State<AppState>, name: String, text: String) -> Res<RouteMeta> {
    import_gpx(&state, &name, &text)
}

#[tauri::command]
pub fn demo_meta() -> RouteMeta {
    meta_for(DEMO_ROUTE_ID.into(), &demo_route(), 0)
}

#[tauri::command]
pub fn badges() -> &'static [Badge] {
    &BADGES
}

// ── Files and links ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn choose_dir(app: AppHandle) -> Res<Option<String>> {
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder()).await.map_err(blocking_err)?;
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn reveal_file(app: AppHandle, state: State<AppState>, path: String) -> Res<()> {
    if !state.store.owns_activity(Path::new(&path)) {
        return Err("Not a WinterMiles activity file.".into());
    }
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) -> Res<()> {
    if !url.starts_with("https://") {
        return Err("Only https links can be opened.".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// ── Devices ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn devices_list(state: State<AppState>) -> DevicesSnapshot {
    state.devices.snapshot()
}

#[tauri::command]
pub async fn ble_scan_start(state: State<'_, AppState>) -> Res<()> {
    state.ble.scan_start(state.devices.clone()).await
}

#[tauri::command]
pub fn ble_scan_stop(state: State<AppState>) {
    state.ble.scan_stop();
}

#[tauri::command]
pub async fn ble_connect(state: State<'_, AppState>, id: String) -> Res<()> {
    state.ble.scan_stop();
    state.ble.connect(state.devices.clone(), &id).await
}

#[tauri::command]
pub fn ant_start(state: State<AppState>) {
    state.ant.start(state.devices.clone());
}

#[tauri::command]
pub fn ant_stop(state: State<AppState>) {
    state.ant.stop();
}

#[tauri::command]
pub fn demo_add(state: State<AppState>) {
    state.demo.add(&state.devices);
}

#[tauri::command]
pub fn demo_nudge(state: State<AppState>, delta: f64) {
    state.demo.nudge(delta);
}

#[tauri::command]
pub fn device_remove(state: State<AppState>, id: String) {
    state.devices.remove(&id);
}

// ── Ride ───────────────────────────────────────────────────────────────────

fn f32_bytes(header: &[u32], parts: &[&[f32]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(header.len() * 4 + parts.iter().map(|p| p.len() * 4).sum::<usize>());
    header.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
    parts.iter().flat_map(|p| p.iter()).for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
    out
}

/// Layout: `[ground vertices, trees, building vertices, water vertices]` as u32, then as f32:
/// ground positions, normals and colours; trees; building positions, normals, colours and UVs;
/// water positions and colours.
fn grid_response(mesh: GridMesh) -> Response {
    let b = &mesh.buildings;
    Response::new(f32_bytes(
        &[(mesh.positions.len() / 3) as u32, (mesh.trees.len() / 6) as u32, b.vertices() as u32, (mesh.water.len() / 3) as u32],
        &[&mesh.positions, &mesh.normals, &mesh.colors, &mesh.trees, &b.positions, &b.normals, &b.colors, &b.uvs, &mesh.water, &mesh.water_colors],
    ))
}

#[tauri::command]
pub async fn ride_start(app: AppHandle, state: State<'_, AppState>, route_id: String) -> Res<RideInit> {
    if let Some(old) = state.session.lock().unwrap().take() {
        old.halt();
    }
    let (session, init) = Session::start(&app, &state.store, &state.http, state.devices.clone(), state.demo.clone(), &route_id).await?;
    *state.session.lock().unwrap() = Some(session);
    Ok(init)
}

/// The route's per-sample arrays as six consecutive f64 blocks: x, z, lat, lon, elevation, gradient.
#[tauri::command]
pub fn ride_route_data(state: State<AppState>) -> Res<Response> {
    let session = state.current_session()?;
    let r = &session.world.route;
    let mut out = Vec::with_capacity(r.n * 48);
    for block in [&r.x, &r.z, &r.lat, &r.lon] {
        block.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
    }
    for block in [&r.ele, &r.grade] {
        block.iter().for_each(|v| out.extend_from_slice(&(*v as f64).to_le_bytes()));
    }
    Ok(Response::new(out))
}

/// Road ribbon: positions (3 per vertex) then UVs (2 per vertex), as f32; `layout.roadSection` vertices per sample.
#[tauri::command]
pub async fn ride_road_mesh(state: State<'_, AppState>) -> Res<Response> {
    let world = state.current_session()?.world.clone();
    let mesh = tauri::async_runtime::spawn_blocking(move || world.road()).await.map_err(blocking_err)?;
    Ok(Response::new(f32_bytes(&[], &[&mesh.positions, &mesh.uvs])))
}

/// Bridges, tunnels and whatever crosses overhead. Layout: `[sunlit vertices, unlit vertices]`
/// as u32, then as f32: sunlit positions, normals and colours; unlit positions and colours.
#[tauri::command]
pub async fn ride_structures(state: State<'_, AppState>) -> Res<Response> {
    let world = state.current_session()?.world.clone();
    let s = tauri::async_runtime::spawn_blocking(move || world.structures()).await.map_err(blocking_err)?;
    Ok(Response::new(f32_bytes(
        &[s.lit.vertices() as u32, s.unlit.vertices() as u32],
        &[&s.lit.positions, &s.lit.normals, &s.lit.colors, &s.unlit.positions, &s.unlit.colors],
    )))
}

#[tauri::command]
pub async fn terrain_chunk(state: State<'_, AppState>, cx: i32, cz: i32) -> Res<Response> {
    let world = state.current_session()?.world.clone();
    tauri::async_runtime::spawn_blocking(move || grid_response(world.chunk(cx, cz))).await.map_err(blocking_err)
}

#[tauri::command]
pub async fn terrain_far(state: State<'_, AppState>, cx: f64, cz: f64) -> Res<Response> {
    let world = state.current_session()?.world.clone();
    tauri::async_runtime::spawn_blocking(move || grid_response(world.far(cx, cz))).await.map_err(blocking_err)
}

#[tauri::command]
pub fn ride_toggle_pause(state: State<AppState>) -> Res<()> {
    state.current_session()?.toggle_pause();
    Ok(())
}

#[tauri::command]
pub fn ride_update_settings(state: State<AppState>, settings: Settings) {
    if let Ok(session) = state.current_session() {
        session.update_settings(settings);
    }
}

/// Ends the ride and saves it. Resolves to `null` if it was too short to keep.
#[tauri::command]
pub async fn ride_finish(state: State<'_, AppState>) -> Res<Option<RideSummary>> {
    let Some(session) = state.session.lock().unwrap().take() else { return Ok(None) };
    session.finish(&state.store)
}

#[tauri::command]
pub fn ride_abort(state: State<AppState>) {
    if let Some(session) = state.session.lock().unwrap().take() {
        session.halt();
    }
}

// ── Strava ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn strava_status(state: State<AppState>) -> StravaStatus {
    strava::status(state.store.dir())
}

#[tauri::command]
pub fn strava_set_app(state: State<AppState>, client_id: String, client_secret: String) -> Res<StravaStatus> {
    strava::set_app(state.store.dir(), &client_id, &client_secret)
}

#[tauri::command]
pub async fn strava_connect(app: AppHandle, state: State<'_, AppState>) -> Res<StravaStatus> {
    strava::connect(state.store.dir(), &state.http, |url| {
        let _ = app.opener().open_url(url, None::<&str>);
    })
    .await
}

#[tauri::command]
pub async fn strava_disconnect(state: State<'_, AppState>) -> Res<StravaStatus> {
    strava::disconnect(state.store.dir(), &state.http).await
}

#[tauri::command]
pub async fn strava_upload(state: State<'_, AppState>, ride_id: String, file_path: String, name: String, description: String) -> Res<u64> {
    // Only files this app wrote are eligible — the UI can't use this to send arbitrary paths.
    let path = PathBuf::from(&file_path);
    if !state.store.owns_activity(&path) {
        return Err("Not a WinterMiles activity file.".into());
    }
    let activity_id = strava::upload(state.store.dir(), &state.http, &path, &name, &description).await?;
    // Remember the link so the ride isn't uploaded twice.
    let mut rides: Vec<RideSummary> = state.store.get("rides").unwrap_or_default();
    if let Some(ride) = rides.iter_mut().find(|r| r.id == ride_id) {
        ride.strava_activity_id = Some(activity_id);
        state.store.set("rides", &rides)?;
    }
    Ok(activity_id)
}

// ── Apple Health ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn health_import(app: AppHandle) -> Res<Option<HealthSummary>> {
    tauri::async_runtime::spawn_blocking(move || {
        let picked = app.dialog().file().set_title("Choose your Apple Health export (export.zip or export.xml)").add_filter("Apple Health export", &["zip", "xml"]).blocking_pick_file();
        let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
        health::import(&path).map(Some)
    })
    .await
    .map_err(blocking_err)?
}

// ── Diagnostics ────────────────────────────────────────────────────────────

/// Set only by the automated check: which screen to open and how long to run before reporting.
#[derive(Serialize)]
pub struct SmokeConfig {
    screen: String,
    ms: u64,
    /// Which camera view to use on the ride screen.
    camera: u32,
}

#[tauri::command]
pub fn smoke_config() -> Option<SmokeConfig> {
    std::env::var_os("WM_SMOKE")?;
    Some(SmokeConfig {
        screen: std::env::var("WM_SMOKE_SCREEN").unwrap_or_else(|_| "ride".into()),
        ms: std::env::var("WM_SMOKE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(9000),
        camera: std::env::var("WM_SMOKE_CAM").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

/// Receives the automated check's screenshot (a PNG data URL) and notes, writes them out and quits.
#[tauri::command]
pub fn smoke_done(app: AppHandle, png: Option<String>, info: String) {
    if let Some(path) = std::env::var_os("WM_SMOKE").map(PathBuf::from) {
        if let Some(bytes) = png.as_deref().and_then(|p| p.split(',').nth(1)).and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()) {
            let _ = std::fs::write(&path, bytes);
        }
        let _ = std::fs::write(path.with_extension("txt"), info);
        app.exit(0);
    }
}

/// Lets the UI surface its errors in the terminal the app was started from.
#[tauri::command]
pub fn log(message: String) {
    eprintln!("[ui] {message}");
}
