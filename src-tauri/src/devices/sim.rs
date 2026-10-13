//! A simulated rider + smart trainer, for trying the app (and developing it) without hardware.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wintermiles_core::devices::{DeviceInfo, DeviceKind, DeviceStatus, Readings, TrainerCommand, Transport};

use super::{Devices, Handle};

pub const DEMO_ID: &str = "sim:demo";

#[derive(Clone, Copy)]
struct State {
    /// Effort on the flat, in watts.
    target: f64,
    grade: f64,
    erg: Option<f64>,
}

#[derive(Default)]
pub struct DemoRider {
    state: Mutex<Option<Arc<Mutex<State>>>>,
}

impl DemoRider {
    pub fn add(&self, devices: &Arc<Devices>) {
        if devices.contains(DEMO_ID) {
            return;
        }
        let state = Arc::new(Mutex::new(State { target: 190.0, grade: 0.0, erg: None }));
        let stop = Arc::new(AtomicBool::new(false));
        *self.state.lock().unwrap() = Some(state.clone());

        let info = DeviceInfo {
            id: DEMO_ID.into(),
            name: "Demo rider (simulated)".into(),
            transport: Transport::Sim,
            kind: DeviceKind::Trainer,
            status: DeviceStatus::Connected,
            controllable: true,
            readings: Readings::default(),
        };
        let (control_state, stop_flag) = (state.clone(), stop.clone());
        devices.add(
            info,
            Handle {
                control: Some(Box::new(move |cmd| {
                    let mut s = control_state.lock().unwrap();
                    match cmd {
                        TrainerCommand::Simulation { grade, .. } => {
                            s.grade = grade;
                            s.erg = None;
                        }
                        TrainerCommand::TargetPower(w) => s.erg = Some(w),
                        TrainerCommand::UserConfig { .. } => {}
                    }
                })),
                stop: Box::new(move || stop_flag.store(true, Ordering::Relaxed)),
            },
        );

        let devices = devices.clone();
        std::thread::spawn(move || {
            let mut hr = 95.0f64;
            let mut phase = 0.0f64;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                phase += 0.25;
                let s = *state.lock().unwrap();
                // Riders naturally push harder uphill and ease off downhill.
                let terrain = 1.0 + (s.grade * 5.0).clamp(-0.6, 0.45);
                let wobble = 1.0 + 0.04 * (phase * 0.9).sin() + 0.03 * (phase * 2.3).sin();
                let power = (s.erg.unwrap_or(s.target * terrain) * wobble).max(0.0);
                let cadence = if power > 5.0 { 88.0 - (s.grade * 220.0).clamp(-8.0, 22.0) + 2.0 * (phase * 0.5).sin() } else { 0.0 };
                hr += (90.0 + power * 0.36 - hr) * 0.008;
                devices.report(DEMO_ID, Readings { power: Some(power), cadence: Some(cadence), hr: Some(hr.round()), wheel_speed: None });
            }
        });
    }

    /// Current flat-road effort, if the demo rider is active.
    pub fn target(&self, devices: &Devices) -> Option<f64> {
        if !devices.contains(DEMO_ID) {
            return None;
        }
        self.state.lock().unwrap().as_ref().map(|s| s.lock().unwrap().target)
    }

    pub fn nudge(&self, delta: f64) {
        if let Some(s) = self.state.lock().unwrap().as_ref() {
            let mut s = s.lock().unwrap();
            s.target = (s.target + delta).clamp(0.0, 600.0);
        }
    }
}
