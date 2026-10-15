//! Strava: OAuth through the system browser and a loopback listener, and FIT upload.
//!
//! Strava requires every API application to have its own client ID/secret, and a
//! secret can't be shipped in an open-source binary. Each user registers a free
//! app at https://www.strava.com/settings/api with callback domain "localhost".

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::{now_ms, secrets};

const PORT: u16 = 53682;

#[derive(Default, Serialize, Deserialize)]
struct Creds {
    client_id: String,
    client_secret: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    athlete: Option<String>,
}

#[derive(Serialize)]
pub struct StravaStatus {
    configured: bool,
    #[serde(rename = "clientId")]
    client_id: String,
    connected: bool,
    athlete: String,
}

fn load(dir: &Path) -> Creds {
    secrets::load(dir).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(dir: &Path, creds: &Creds) -> Result<(), String> {
    secrets::save(dir, &serde_json::to_string(creds).map_err(|e| e.to_string())?)
}

fn to_status(c: &Creds) -> StravaStatus {
    StravaStatus {
        configured: !c.client_id.is_empty() && !c.client_secret.is_empty(),
        client_id: c.client_id.clone(),
        connected: c.refresh_token.is_some(),
        athlete: c.athlete.clone().unwrap_or_default(),
    }
}

pub fn status(dir: &Path) -> StravaStatus {
    to_status(&load(dir))
}

pub fn set_app(dir: &Path, client_id: &str, client_secret: &str) -> Result<StravaStatus, String> {
    let prev = load(dir);
    let secret = if client_secret.trim().is_empty() { prev.client_secret } else { client_secret.trim().to_string() };
    let next = Creds { client_id: client_id.trim().to_string(), client_secret: secret, ..Default::default() };
    save(dir, &next)?;
    Ok(to_status(&next))
}

async fn token_request(http: &reqwest::Client, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let res = http.post("https://www.strava.com/oauth/token").form(form).send().await.map_err(|e| format!("Could not reach Strava: {e}"))?;
    let status = res.status();
    let json: serde_json::Value = res.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Strava token request failed ({}): {}", status.as_u16(), json["message"].as_str().unwrap_or("unknown error")));
    }
    Ok(json)
}

fn apply_tokens(creds: &mut Creds, tok: &serde_json::Value) {
    creds.access_token = tok["access_token"].as_str().map(str::to_string);
    creds.refresh_token = tok["refresh_token"].as_str().map(str::to_string);
    creds.expires_at = tok["expires_at"].as_u64();
}

/// Waits for Strava's redirect on the loopback port and returns the authorization code.
async fn await_callback(listener: TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let path = request.split_whitespace().nth(1).unwrap_or("/");
        let Ok(url) = reqwest::Url::parse(&format!("http://localhost{path}")) else { continue };
        if url.path() != "/callback" {
            let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let param = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        let code = param("code").filter(|_| param("state").as_deref() == Some(state));
        let granted = code.is_some() && param("scope").unwrap_or_default().contains("activity:write");
        let (title, detail) = if granted {
            ("WinterMiles is connected to Strava", "You can close this tab.")
        } else if code.is_some() {
            ("Strava authorization failed", "The \u{201c}Upload your activities\u{201d} permission is required.")
        } else {
            ("Strava authorization failed", "The request was denied or invalid.")
        };
        let body = format!("<body style=\"font-family:system-ui;padding:3em;text-align:center\"><h2>{title}</h2><p>{detail}</p></body>");
        let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
        let _ = socket.shutdown().await;
        return match (granted, code) {
            (true, Some(code)) => Ok(code),
            (false, Some(_)) => Err("Upload permission was not granted.".into()),
            _ => Err("Strava authorization was denied.".into()),
        };
    }
}

/// Runs the OAuth authorization-code flow. `open_browser` is handed the URL to show the user.
pub async fn connect(dir: &Path, http: &reqwest::Client, open_browser: impl FnOnce(&str)) -> Result<StravaStatus, String> {
    let mut creds = load(dir);
    if creds.client_id.is_empty() || creds.client_secret.is_empty() {
        return Err("Enter your Strava API client ID and secret first.".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", PORT)).await.map_err(|e| format!("Could not listen for Strava's reply on port {PORT}: {e}"))?;
    let state = uuid::Uuid::new_v4().simple().to_string();
    let redirect = format!("http://localhost:{PORT}/callback");
    let auth = reqwest::Url::parse_with_params(
        "https://www.strava.com/oauth/authorize",
        &[("client_id", creds.client_id.as_str()), ("response_type", "code"), ("redirect_uri", &redirect), ("approval_prompt", "auto"), ("scope", "read,activity:read_all,activity:write"), ("state", &state)],
    )
    .map_err(|e| e.to_string())?;
    open_browser(auth.as_str());

    let code = tokio::time::timeout(Duration::from_secs(300), await_callback(listener, &state)).await.map_err(|_| "Timed out waiting for Strava authorization.".to_string())??;
    let tok = token_request(http, &[("client_id", &creds.client_id), ("client_secret", &creds.client_secret), ("code", &code), ("grant_type", "authorization_code")]).await?;
    apply_tokens(&mut creds, &tok);
    let name = [tok["athlete"]["firstname"].as_str(), tok["athlete"]["lastname"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(" ");
    creds.athlete = Some(name);
    save(dir, &creds)?;
    Ok(to_status(&creds))
}

pub async fn disconnect(dir: &Path, http: &reqwest::Client) -> Result<StravaStatus, String> {
    let creds = load(dir);
    if let Some(token) = &creds.access_token {
        let _ = http.post("https://www.strava.com/oauth/deauthorize").bearer_auth(token).send().await;
    }
    let next = Creds { client_id: creds.client_id, client_secret: creds.client_secret, ..Default::default() };
    save(dir, &next)?;
    Ok(to_status(&next))
}

async fn access_token(dir: &Path, http: &reqwest::Client) -> Result<String, String> {
    let mut creds = load(dir);
    let refresh = creds.refresh_token.clone().ok_or("Not connected to Strava.")?;
    if let (Some(token), Some(expires)) = (&creds.access_token, creds.expires_at) {
        if expires > now_ms() / 1000 + 120 {
            return Ok(token.clone());
        }
    }
    let tok = token_request(http, &[("client_id", &creds.client_id), ("client_secret", &creds.client_secret), ("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await?;
    apply_tokens(&mut creds, &tok);
    save(dir, &creds)?;
    creds.access_token.ok_or_else(|| "Strava did not return an access token.".into())
}

/// Uploads a FIT file and waits for Strava to process it. Resolves to the new activity ID.
pub async fn upload(dir: &Path, http: &reqwest::Client, file: &Path, name: &str, description: &str) -> Result<u64, String> {
    let token = access_token(dir, http).await?;
    let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or("ride.fit").to_string();
    let bytes = tokio::task::spawn_blocking({
        let file = file.to_path_buf();
        move || std::fs::read(file)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("Could not read the ride file: {e}"))?;
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(bytes).file_name(file_name.clone()))
        .text("data_type", "fit")
        .text("name", name.to_string())
        .text("description", description.to_string())
        .text("sport_type", "VirtualRide")
        .text("external_id", file_name);

    let res = http.post("https://www.strava.com/api/v3/uploads").bearer_auth(&token).multipart(form).send().await.map_err(|e| format!("Could not reach Strava: {e}"))?;
    let status = res.status();
    let mut json: serde_json::Value = res.json().await.unwrap_or_default();
    if !status.is_success() {
        let reason = json["error"].as_str().or(json["message"].as_str()).unwrap_or("unknown error");
        return Err(format!("Strava upload failed ({}): {reason}", status.as_u16()));
    }
    for _ in 0..30 {
        if let Some(e) = json["error"].as_str() {
            return Err(format!("Strava rejected the file: {e}"));
        }
        if let Some(id) = json["activity_id"].as_u64() {
            return Ok(id);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Some(upload_id) = json["id"].as_u64() else { break };
        if let Ok(poll) = http.get(format!("https://www.strava.com/api/v3/uploads/{upload_id}")).bearer_auth(&token).send().await {
            json = poll.json().await.unwrap_or(json);
        }
    }
    Err("Strava is still processing the upload — check your Strava feed in a minute.".into())
}

async fn get_json(http: &reqwest::Client, token: &str, url: &str) -> Result<serde_json::Value, String> {
    let res = http.get(url).bearer_auth(token).send().await.map_err(|e| format!("Could not reach Strava: {e}"))?;
    let status = res.status();
    let json: serde_json::Value = res.json().await.unwrap_or_default();
    match status.as_u16() {
        200..=299 => Ok(json),
        401 | 403 => Err("Strava refused: reconnect Strava in Settings so WinterMiles may read your activities and segments.".into()),
        429 => Err("Strava's rate limit was reached — try again in 15 minutes.".into()),
        code => Err(format!("Strava request failed ({code}): {}", json["message"].as_str().unwrap_or("unknown error"))),
    }
}

/// A Strava segment's shape and details, as found near a route.
pub struct FoundSegment {
    pub id: u64,
    pub name: String,
    pub points: String,
    pub distance: f64,
    pub avg_grade: f64,
}

/// Segments Strava suggests in each part of the route, plus the rider's starred segments.
/// `boxes` are `[south, west, north, east]` areas covering the route.
pub async fn segments_near(dir: &Path, http: &reqwest::Client, boxes: &[[f64; 4]]) -> Result<Vec<FoundSegment>, String> {
    let token = access_token(dir, http).await?;
    let mut found: Vec<FoundSegment> = Vec::new();
    let mut push = |v: &serde_json::Value| {
        let Some(id) = v["id"].as_u64() else { return };
        if found.iter().any(|f| f.id == id) {
            return;
        }
        let points = v["points"].as_str().or(v["map"]["polyline"].as_str()).unwrap_or("").to_string();
        if points.is_empty() {
            return;
        }
        found.push(FoundSegment {
            id,
            name: v["name"].as_str().unwrap_or("Segment").trim().to_string(),
            points,
            distance: v["distance"].as_f64().unwrap_or(0.0),
            avg_grade: v["avg_grade"].as_f64().or(v["average_grade"].as_f64()).unwrap_or(0.0) / 100.0,
        });
    };
    for b in boxes {
        let url = format!("https://www.strava.com/api/v3/segments/explore?bounds={:.5},{:.5},{:.5},{:.5}&activity_type=riding", b[0], b[1], b[2], b[3]);
        let json = get_json(http, &token, &url).await?;
        json["segments"].as_array().into_iter().flatten().for_each(&mut push);
    }
    let starred = get_json(http, &token, "https://www.strava.com/api/v3/segments/starred?per_page=200").await?;
    starred.as_array().into_iter().flatten().for_each(&mut push);
    Ok(found)
}

/// The rider's recent rides, for picking one to race as a ghost.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    id: u64,
    name: String,
    start: String,
    distance: f64,
    moving_time: f64,
}

pub async fn recent_rides(dir: &Path, http: &reqwest::Client) -> Result<Vec<ActivityItem>, String> {
    let token = access_token(dir, http).await?;
    let json = get_json(http, &token, "https://www.strava.com/api/v3/athlete/activities?per_page=60").await?;
    Ok(json
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["type"].as_str().is_some_and(|t| t.contains("Ride")) && a["start_latlng"].as_array().is_some_and(|l| !l.is_empty()))
        .filter_map(|a| {
            Some(ActivityItem {
                id: a["id"].as_u64()?,
                name: a["name"].as_str().unwrap_or("Ride").to_string(),
                start: a["start_date_local"].as_str().unwrap_or("").to_string(),
                distance: a["distance"].as_f64().unwrap_or(0.0),
                moving_time: a["moving_time"].as_f64().unwrap_or(0.0),
            })
        })
        .collect())
}

/// An activity's timed GPS track as `(unix seconds, lat, lon)`.
pub async fn activity_track(dir: &Path, http: &reqwest::Client, id: u64) -> Result<(String, Vec<(f64, f64, f64)>), String> {
    let token = access_token(dir, http).await?;
    let info = get_json(http, &token, &format!("https://www.strava.com/api/v3/activities/{id}")).await?;
    let start = info["start_date"].as_str().and_then(wintermiles_core::gpx::parse_iso_time).unwrap_or(0.0);
    let streams = get_json(http, &token, &format!("https://www.strava.com/api/v3/activities/{id}/streams?keys=latlng,time&key_by_type=true")).await?;
    let (Some(ll), Some(tt)) = (streams["latlng"]["data"].as_array(), streams["time"]["data"].as_array()) else {
        return Err("That activity has no GPS track.".into());
    };
    let track = ll
        .iter()
        .zip(tt)
        .filter_map(|(p, t)| Some((start + t.as_f64()?, p[0].as_f64()?, p[1].as_f64()?)))
        .collect();
    Ok((info["name"].as_str().unwrap_or("Strava ride").to_string(), track))
}
