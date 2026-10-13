// No console window alongside the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod buildings;
mod commands;
mod devices;
mod health;
mod secrets;
mod session;
mod store;
mod strava;
mod terrain;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::Manager;

use devices::ant::Ant;
use devices::ble::Ble;
use devices::sim::DemoRider;
use devices::Devices;
use session::Session;
use store::Store;

/// Unix time in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub struct AppState {
    pub store: Store,
    pub devices: Arc<Devices>,
    pub ble: Ble,
    pub ant: Ant,
    pub demo: Arc<DemoRider>,
    pub session: Mutex<Option<Arc<Session>>>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn current_session(&self) -> Result<Arc<Session>, String> {
        self.session.lock().unwrap().clone().ok_or_else(|| "No ride in progress.".to_string())
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // WM_DATA_DIR redirects all storage, for tests and portable installs.
            let data_dir = match std::env::var_os("WM_DATA_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => app.path().app_data_dir()?,
            };
            let http = reqwest::Client::builder().user_agent("WinterMiles/0.2 (open-source indoor cycling app)").connect_timeout(std::time::Duration::from_secs(8)).build()?;
            app.manage(AppState {
                store: Store::new(data_dir),
                devices: Arc::new(Devices::new(app.handle().clone())),
                ble: Ble::default(),
                ant: Ant::default(),
                demo: Arc::new(DemoRider::default()),
                session: Mutex::new(None),
                http,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::kv_get,
            commands::kv_set,
            commands::kv_del,
            commands::routes_import_dialog,
            commands::routes_import_text,
            commands::demo_meta,
            commands::badges,
            commands::choose_dir,
            commands::reveal_file,
            commands::open_external,
            commands::devices_list,
            commands::ble_scan_start,
            commands::ble_scan_stop,
            commands::ble_connect,
            commands::ant_start,
            commands::ant_stop,
            commands::demo_add,
            commands::demo_nudge,
            commands::device_remove,
            commands::ride_start,
            commands::ride_route_data,
            commands::ride_road_mesh,
            commands::terrain_chunk,
            commands::terrain_far,
            commands::ride_toggle_pause,
            commands::ride_update_settings,
            commands::ride_finish,
            commands::ride_abort,
            commands::strava_status,
            commands::strava_set_app,
            commands::strava_connect,
            commands::strava_disconnect,
            commands::strava_upload,
            commands::health_import,
            commands::smoke_config,
            commands::smoke_done,
            commands::log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running WinterMiles");
}
