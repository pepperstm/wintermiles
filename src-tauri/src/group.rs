//! Group rides: a connection to a relay server (see `crates/relay`) that shares
//! this rider's position with everyone in the same room and brings theirs back.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::AppHandle;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::store::Store;

const COLORS: [u32; 8] = [0x3ec6ff, 0xffc93e, 0xff7ad9, 0x7dff8a, 0xb38bff, 0xff9f43, 0x2ee6c5, 0xf5f5f5];

pub enum Update {
    Rider { id: String, name: String, color: u32, distance: f64, speed: f64 },
    Left(String),
}

pub struct GroupLink {
    out: mpsc::UnboundedSender<String>,
    updates: Mutex<Vec<Update>>,
    status: Mutex<String>,
    /// Identifies the route, so riders on a different route in the same room are ignored.
    key: String,
    name: String,
}

/// A short fingerprint of a route's GPX text (FNV-1a).
pub fn route_key(gpx: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in gpx.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[derive(serde::Deserialize)]
struct StoredRoute {
    name: String,
    gpx: String,
}

impl GroupLink {
    pub fn join(_app: AppHandle, url: &str, room: &str, name: &str, route_id: &str, store: &Store) -> Arc<GroupLink> {
        let stored: Option<StoredRoute> = store.get(&format!("route-{route_id}"));
        let key = stored.as_ref().map_or_else(|| route_id.to_string(), |r| route_key(&r.gpx));
        let (tx, rx) = mpsc::unbounded_channel();
        let link = Arc::new(GroupLink { out: tx, updates: Mutex::default(), status: Mutex::new("Connecting to the group…".into()), key: key.clone(), name: name.to_string() });
        let hello = vec![
            json!({ "t": "join", "room": room }).to_string(),
            json!({ "t": "route", "name": stored.as_ref().map(|r| r.name.as_str()).unwrap_or(""), "gpx": stored.as_ref().map(|r| r.gpx.as_str()).unwrap_or(""), "key": key }).to_string(),
        ];
        tauri::async_runtime::spawn(run(link.clone(), url.to_string(), hello, rx));
        link
    }

    pub fn send(&self, distance: f64, speed: f64, power: f64) {
        let _ = self.out.send(json!({ "t": "pos", "name": self.name, "key": self.key, "d": (distance * 10.0).round() / 10.0, "v": (speed * 100.0).round() / 100.0, "p": power.round() }).to_string());
    }

    pub fn take_updates(&self) -> Vec<Update> {
        std::mem::take(&mut *self.updates.lock().unwrap())
    }

    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    pub fn leave(&self) {
        let _ = self.out.send(String::new());
    }
}

async fn run(link: Arc<GroupLink>, url: String, hello: Vec<String>, mut rx: mpsc::UnboundedReceiver<String>) {
    let set = |s: String| *link.status.lock().unwrap() = s;
    let ws = match tokio::time::timeout(Duration::from_secs(8), tokio_tungstenite::connect_async(&url)).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => return set(format!("Could not reach the group relay: {e}")),
        Err(_) => return set("Could not reach the group relay (timed out).".into()),
    };
    let (mut sink, mut source) = ws.split();
    for m in hello {
        if sink.send(Message::text(m)).await.is_err() {
            return set("Lost the connection to the group relay.".into());
        }
    }
    let mut riders = std::collections::HashSet::new();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(m) if !m.is_empty() => { if sink.send(Message::text(m)).await.is_err() { break } }
                _ => { let _ = sink.close().await; return }
            },
            msg = source.next() => {
                let Some(Ok(Message::Text(text))) = msg else {
                    if msg.is_none() || matches!(msg, Some(Err(_))) { break } else { continue }
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match v["t"].as_str() {
                    Some("welcome") => set(format!("In the group · {} rider{}", v["riders"], if v["riders"].as_u64() == Some(1) { "" } else { "s" })),
                    Some("error") => set(v["message"].as_str().unwrap_or("The relay refused to join.").to_string()),
                    Some("pos") if v["key"].as_str() == Some(link.key.as_str()) => {
                        let id = format!("r-{}", v["id"]);
                        let n = v["id"].as_u64().unwrap_or(0) as usize;
                        riders.insert(id.clone());
                        set(format!("In the group · {} other rider{}", riders.len(), if riders.len() == 1 { "" } else { "s" }));
                        let name: String = v["name"].as_str().unwrap_or("Rider").chars().take(24).collect();
                        link.updates.lock().unwrap().push(Update::Rider { id, name, color: COLORS[n % COLORS.len()], distance: v["d"].as_f64().unwrap_or(0.0), speed: v["v"].as_f64().unwrap_or(0.0) });
                    }
                    Some("left") => {
                        let id = format!("r-{}", v["id"]);
                        riders.remove(&id);
                        link.updates.lock().unwrap().push(Update::Left(id));
                    }
                    _ => {}
                }
            }
        }
    }
    set("Lost the connection to the group relay.".into());
}

/// Fetches the route a room is riding, as `(name, gpx)`, without joining it.
pub async fn peek(url: &str, room: &str) -> Result<(String, String), String> {
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(8), tokio_tungstenite::connect_async(url))
        .await
        .map_err(|_| "Could not reach the group relay (timed out).".to_string())?
        .map_err(|e| format!("Could not reach the group relay: {e}"))?;
    ws.send(Message::text(json!({ "t": "peek", "room": room }).to_string())).await.map_err(|e| e.to_string())?;
    let reply = tokio::time::timeout(Duration::from_secs(20), ws.next()).await.map_err(|_| "The relay did not answer.".to_string())?;
    let _ = ws.close(None).await;
    let Some(Ok(Message::Text(text))) = reply else { return Err("The relay did not answer.".into()) };
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    match (v["t"].as_str(), v["gpx"].as_str()) {
        (Some("route"), Some(gpx)) if !gpx.is_empty() => Ok((v["name"].as_str().unwrap_or("Group ride").to_string(), gpx.to_string())),
        _ => Err(format!("Nobody is riding in room {room} yet — the host needs to start the ride first.")),
    }
}
