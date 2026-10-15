//! Riders in the same room see each other; the room's route can be fetched before joining.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Value {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.expect("no reply").unwrap().unwrap();
    serde_json::from_str(msg.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn relays_positions_and_shares_the_route() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(wintermiles_relay::serve(listener));

    let (mut host, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut host, json!({ "t": "join", "room": "abc1" })).await;
    assert_eq!(recv(&mut host).await["t"], "welcome");
    send(&mut host, json!({ "t": "route", "name": "Loop", "gpx": "<gpx/>", "key": "k" })).await;

    // Someone else can read the route without joining; a missing room says so.
    let (mut peek, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut peek, json!({ "t": "peek", "room": "ABC1" })).await;
    let route = recv(&mut peek).await;
    assert_eq!(route["name"], "Loop");
    assert_eq!(route["gpx"], "<gpx/>");
    send(&mut peek, json!({ "t": "peek", "room": "NOPE" })).await;
    assert_eq!(recv(&mut peek).await["t"], "noroom");

    let (mut friend, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut friend, json!({ "t": "join", "room": "ABC1" })).await;
    assert_eq!(recv(&mut friend).await["riders"], 2);

    send(&mut friend, json!({ "t": "pos", "name": "Friend", "key": "k", "d": 120.5, "v": 8.0, "p": 210 })).await;
    let pos = recv(&mut host).await;
    assert_eq!(pos["t"], "pos");
    assert_eq!(pos["name"], "Friend");
    assert_eq!(pos["d"], 120.5);
    let friend_id = pos["id"].clone();

    drop(friend);
    let left = recv(&mut host).await;
    assert_eq!(left["t"], "left");
    assert_eq!(left["id"], friend_id);

    // Bad room codes are refused.
    let (mut bad, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut bad, json!({ "t": "join", "room": "no spaces!" })).await;
    assert_eq!(recv(&mut bad).await["t"], "error");
}
