//! WinterMiles group-ride relay.
//!
//! Riders connect over WebSocket and join a room by code. The first rider to
//! join shares the route (its GPX), later riders download it, and from then
//! on every rider's position is passed on to everyone else in the room.
//! Nothing is stored: a room disappears when its last rider leaves.
//!

use std::collections::HashMap;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// Routes are passed around as GPX; this bounds a single message.
const MAX_MESSAGE: usize = 8 << 20;
const MAX_ROOMS: usize = 1000;
const MAX_RIDERS: usize = 50;

#[derive(Default)]
struct Room {
    /// `{"t":"route", "name", "gpx", "key"}` as sent by the host.
    route: Option<Value>,
    riders: HashMap<u64, mpsc::UnboundedSender<Message>>,
}

type Rooms = Arc<Mutex<HashMap<String, Room>>>;

pub async fn serve(listener: TcpListener) -> std::io::Result<()> {
    let rooms: Rooms = Arc::default();
    let mut next_id = 0u64;
    loop {
        let (stream, _) = listener.accept().await?;
        next_id += 1;
        tokio::spawn(handle(stream, rooms.clone(), next_id));
    }
}

fn valid_room(code: &str) -> bool {
    (1..=32).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

async fn handle(stream: TcpStream, rooms: Rooms, id: u64) {
    let config = WebSocketConfig { max_message_size: Some(MAX_MESSAGE), max_frame_size: Some(MAX_MESSAGE), ..Default::default() };
    let Ok(ws) = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await else { return };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    let mut room_code: Option<String> = None;
    while let Some(Ok(msg)) = source.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(mut v) = serde_json::from_str::<Value>(&text) else { continue };
        match v["t"].as_str() {
            // Look at a room's route without joining it.
            Some("peek") => {
                let code = v["room"].as_str().unwrap_or("").to_ascii_uppercase();
                let rooms = rooms.lock().await;
                let reply = rooms.get(&code).and_then(|r| r.route.clone()).unwrap_or_else(|| json!({ "t": "noroom" }));
                let _ = tx.send(Message::text(reply.to_string()));
            }
            Some("join") if room_code.is_none() => {
                let code = v["room"].as_str().unwrap_or("").to_ascii_uppercase();
                if !valid_room(&code) {
                    let _ = tx.send(Message::text(json!({ "t": "error", "message": "Room codes are letters, digits and dashes." }).to_string()));
                    continue;
                }
                let mut rooms = rooms.lock().await;
                if !rooms.contains_key(&code) && rooms.len() >= MAX_ROOMS {
                    let _ = tx.send(Message::text(json!({ "t": "error", "message": "The relay is full." }).to_string()));
                    continue;
                }
                let room = rooms.entry(code.clone()).or_default();
                if room.riders.len() >= MAX_RIDERS {
                    let _ = tx.send(Message::text(json!({ "t": "error", "message": "That room is full." }).to_string()));
                    continue;
                }
                room.riders.insert(id, tx.clone());
                let _ = tx.send(Message::text(json!({ "t": "welcome", "id": id, "riders": room.riders.len() }).to_string()));
                room_code = Some(code);
            }
            Some("route") => {
                let Some(code) = &room_code else { continue };
                let mut rooms = rooms.lock().await;
                if let Some(room) = rooms.get_mut(code) {
                    // The first route shared in a room is the room's route.
                    if room.route.is_none() {
                        room.route = Some(v);
                    }
                }
            }
            Some("pos") => {
                let Some(code) = &room_code else { continue };
                v["id"] = json!(id);
                let out = Message::text(v.to_string());
                let rooms = rooms.lock().await;
                if let Some(room) = rooms.get(code) {
                    for (other, sender) in &room.riders {
                        if *other != id {
                            let _ = sender.send(out.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(code) = room_code {
        let mut rooms = rooms.lock().await;
        if let Some(room) = rooms.get_mut(&code) {
            room.riders.remove(&id);
            let left = Message::text(json!({ "t": "left", "id": id }).to_string());
            for sender in room.riders.values() {
                let _ = sender.send(left.clone());
            }
            if room.riders.is_empty() {
                rooms.remove(&code);
            }
        }
    }
    writer.abort();
}
