//! Connected sensors: the shared registry plus one module per transport.

pub mod ant;
pub mod ble;
pub mod sim;

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wintermiles_core::devices::{DeviceInfo, DeviceStatus, Hub, Readings, Telemetry, TrainerCommand};

use crate::now_ms;

/// How the registry reaches the task or thread that owns a device.
pub struct Handle {
    pub control: Option<Box<dyn Fn(TrainerCommand) + Send + Sync>>,
    pub stop: Box<dyn Fn() + Send + Sync>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AntStatus {
    /// "off", "opening", "scanning" or "error"
    pub state: &'static str,
    pub message: String,
}

#[derive(Clone, Serialize)]
pub struct DevicesSnapshot {
    pub devices: Vec<DeviceInfo>,
    pub ant: AntStatus,
}

pub struct Devices {
    app: AppHandle,
    hub: Mutex<Hub>,
    handles: Mutex<HashMap<String, Handle>>,
    ant_status: Mutex<AntStatus>,
}

impl Devices {
    pub fn new(app: AppHandle) -> Self {
        Self { app, hub: Mutex::default(), handles: Mutex::default(), ant_status: Mutex::new(AntStatus { state: "off", message: String::new() }) }
    }

    pub fn snapshot(&self) -> DevicesSnapshot {
        DevicesSnapshot { devices: self.hub.lock().unwrap().snapshot(now_ms()), ant: self.ant_status.lock().unwrap().clone() }
    }

    /// Tells the UI the device list or a status changed.
    pub fn changed(&self) {
        let _ = self.app.emit("devices:changed", self.snapshot());
    }

    pub fn add(&self, info: DeviceInfo, handle: Handle) -> bool {
        let id = info.id.clone();
        if !self.hub.lock().unwrap().add(info) {
            return false;
        }
        self.handles.lock().unwrap().insert(id, handle);
        self.changed();
        true
    }

    pub fn contains(&self, id: &str) -> bool {
        self.hub.lock().unwrap().contains(id)
    }

    /// Stops the device's task and forgets it.
    pub fn remove(&self, id: &str) {
        // Take the handle out before calling it: `stop` may call back into the registry.
        let handle = self.handles.lock().unwrap().remove(id);
        if let Some(h) = handle {
            (h.stop)();
        }
        if self.hub.lock().unwrap().remove(id) {
            self.changed();
        }
    }

    pub fn report(&self, id: &str, readings: Readings) {
        self.hub.lock().unwrap().report(id, readings, now_ms());
    }

    pub fn set_status(&self, id: &str, status: DeviceStatus) {
        if self.hub.lock().unwrap().set_status(id, status) {
            self.changed();
        }
    }

    /// Updates what a device turned out to be once its services are known.
    pub fn update(&self, id: &str, f: impl FnOnce(&mut DeviceInfo)) {
        if let Some(d) = self.hub.lock().unwrap().get_mut(id) {
            f(d);
        }
        self.changed();
    }

    pub fn telemetry(&self, use_virtual_power: bool) -> Telemetry {
        self.hub.lock().unwrap().telemetry(use_virtual_power, now_ms())
    }

    pub fn is_empty(&self) -> bool {
        self.hub.lock().unwrap().is_empty()
    }

    pub fn has_trainer(&self) -> bool {
        self.hub.lock().unwrap().trainer().is_some()
    }

    /// Sends a command to the connected controllable trainer, if there is one.
    pub fn command_trainer(&self, cmd: TrainerCommand) {
        let Some(id) = self.hub.lock().unwrap().trainer().map(str::to_string) else { return };
        if let Some(control) = self.handles.lock().unwrap().get(&id).and_then(|h| h.control.as_ref()) {
            control(cmd);
        }
    }

    pub fn set_ant_status(&self, state: &'static str, message: impl Into<String>) {
        *self.ant_status.lock().unwrap() = AntStatus { state, message: message.into() };
        self.changed();
    }

    pub fn ant_state(&self) -> &'static str {
        self.ant_status.lock().unwrap().state
    }

    pub fn app(&self) -> &AppHandle {
        &self.app
    }
}
