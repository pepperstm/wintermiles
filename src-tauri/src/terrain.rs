//! Downloads the open "Terrarium" elevation tiles (Tilezen/Mapzen data hosted on
//! AWS Open Data — no API key needed) that cover a route.

use std::time::Duration;

use futures::future::join_all;
use wintermiles_core::gpx::Route;
use wintermiles_core::world::{Dem, TILE};

const TILE_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";

fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut decoder = png::Decoder::new(bytes);
    // Heights are packed into RGB, so expand palettes/bit depths to plain 8-bit channels.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.width as usize != TILE || info.height as usize != TILE {
        return None;
    }
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return None,
    };
    buf.truncate(info.buffer_size());
    Some((buf, channels))
}

/// Resolves to `None` when offline, too slow, or the route is too spread out —
/// in which case the world falls back to procedurally generated hills.
pub async fn load_dem(client: &reqwest::Client, route: &Route) -> Option<Dem> {
    let (zoom, keys) = Dem::plan(route)?;
    let fetch = |(x, y): (i64, i64)| async move {
        let res = client.get(format!("{TILE_URL}/{zoom}/{x}/{y}.png")).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let bytes = res.bytes().await.ok()?;
        decode_png(&bytes).map(|(pixels, channels)| ((x, y), pixels, channels))
    };
    let fetched = tokio::time::timeout(Duration::from_secs(15), join_all(keys.iter().copied().map(fetch))).await.ok()?;
    let mut dem = Dem::new(zoom);
    for (key, pixels, channels) in fetched.into_iter().flatten() {
        dem.insert_rgb(key, &pixels, channels);
    }
    (dem.tile_count() as f64 >= keys.len() as f64 * 0.6).then_some(dem)
}
