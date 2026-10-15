//! Street-level photos along a route, from Mapillary (needs the rider's own free client token).

use std::time::Duration;

use base64::Engine;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use wintermiles_core::gpx::Route;
use wintermiles_core::segments::{pick_photos, Photo, PhotoCandidate};

use crate::now_ms;
use crate::store::Store;

#[derive(Serialize, Deserialize)]
struct Cached {
    fetched_at: u64,
    photos: Vec<Photo>,
}

/// Small boxes `[west, south, east, north]` covering the route; Mapillary only answers for small areas.
fn boxes(route: &Route) -> Vec<[f64; 4]> {
    let mut out = Vec::new();
    let mut d = 0.0;
    while d < route.total {
        let (a, b) = ((d / route.step) as usize, (((d + 400.0) / route.step) as usize).min(route.n - 1));
        let lat = &route.lat[a..=b];
        let lon = &route.lon[a..=b];
        let pad = 0.0002;
        out.push([
            lon.iter().copied().fold(180.0, f64::min) - pad,
            lat.iter().copied().fold(90.0, f64::min) - pad,
            lon.iter().copied().fold(-180.0, f64::max) + pad,
            lat.iter().copied().fold(-90.0, f64::max) + pad,
        ]);
        d += 400.0;
    }
    out
}

async fn fetch_box(http: &reqwest::Client, token: &str, b: [f64; 4]) -> Result<Vec<PhotoCandidate>, String> {
    let url = format!(
        "https://graph.mapillary.com/images?access_token={token}&fields=id,computed_geometry,computed_compass_angle,thumb_1024_url,captured_at&bbox={:.6},{:.6},{:.6},{:.6}&limit=200",
        b[0], b[1], b[2], b[3]
    );
    let res = http.get(url).timeout(Duration::from_secs(20)).send().await.map_err(|e| format!("Could not reach Mapillary: {e}"))?;
    let status = res.status();
    let json: serde_json::Value = res.json().await.unwrap_or_default();
    if status.as_u16() == 401 || status.as_u16() == 403 || json["error"]["code"].as_u64() == Some(190) {
        return Err("Mapillary refused the client token — check it in Settings.".into());
    }
    if !status.is_success() {
        return Err(format!("Mapillary answered {status}"));
    }
    Ok(json["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let c = p["computed_geometry"]["coordinates"].as_array()?;
            Some((
                p["id"].as_str()?.to_string(),
                c.get(1)?.as_f64()?,
                c.first()?.as_f64()?,
                p["computed_compass_angle"].as_f64()?,
                p["thumb_1024_url"].as_str()?.to_string(),
                p["captured_at"].as_u64().unwrap_or(0),
            ))
        })
        .collect())
}

/// Photos along the route, every 25 m or so where Mapillary has them. Cached for a month.
pub async fn along(http: &reqwest::Client, store: &Store, route_id: &str, route: &Route, token: &str) -> Result<Vec<Photo>, String> {
    let key = format!("photos-{route_id}");
    if let Some(c) = store.get::<Cached>(&key).filter(|c| now_ms() - c.fetched_at < 30 * 86_400_000) {
        return Ok(c.photos);
    }
    if token.trim().is_empty() {
        return Err("Add a Mapillary client token in Settings to see street-level photos.".into());
    }
    let mut boxes = boxes(route);
    boxes.truncate(400);
    let results: Vec<Result<Vec<PhotoCandidate>, String>> = stream::iter(boxes).map(|b| fetch_box(http, token, b)).buffer_unordered(6).collect().await;
    // A bad token fails every request; report that rather than "no photos".
    if let Some(Err(e)) = results.iter().find(|r| r.as_ref().is_err_and(|e| e.contains("token"))) {
        return Err(e.clone());
    }
    let candidates: Vec<PhotoCandidate> = results.into_iter().flatten().flatten().collect();
    let photos = pick_photos(route, &candidates, 25.0);
    store.set(&key, &Cached { fetched_at: now_ms(), photos: photos.clone() })?;
    Ok(photos)
}

/// Downloads one photo as a data URL (the page itself may only load images from the app).
pub async fn image(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().unwrap_or("");
    if parsed.scheme() != "https" || !(host.ends_with(".fbcdn.net") || host.ends_with(".mapillary.com")) {
        return Err("Not a Mapillary photo address.".into());
    }
    let res = http.get(parsed).timeout(Duration::from_secs(15)).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("Photo download failed ({})", res.status()));
    }
    let bytes = res.bytes().await.map_err(|e| e.to_string())?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}
