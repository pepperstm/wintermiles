//! Fetches building outlines along a route from OpenStreetMap and caches them,
//! so each route is normally downloaded only once.
//!
//! Two sources, in order of preference:
//! 1. The Overpass API — full detail (mapped heights, storeys, building types),
//!    but served by volunteer-run machines that are often overloaded.
//! 2. OpenStreetMap's own vector tiles — outlines only, but fast and dependable.

use std::time::Duration;

use futures::future::select_ok;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use wintermiles_core::gpx::Route;
use wintermiles_core::world::{building_tiles, overpass_query, parse_building_tile, parse_overpass, Footprint, BUILDING_ZOOM};

use crate::now_ms;
use crate::store::Store;

/// Public Overpass servers. The first stretch of a route is asked of all of them at once and the
/// first good answer wins; the rest of the route then goes to that server alone.
const OVERPASS: [&str; 3] = ["https://overpass-api.de/api/interpreter", "https://overpass.openstreetmap.fr/api/interpreter", "https://overpass.private.coffee/api/interpreter"];
const VECTOR_TILES: &str = "https://vector.openstreetmap.org/shortbread_v1";
/// Buildings are fetched this far to either side of the road.
const RADIUS_M: f64 = 350.0;
const SPACING_M: f64 = 100.0;
/// Long routes are asked for in stretches, keeping each request small enough for a public server.
const STRETCH_M: f64 = 20_000.0;
/// How long a ride start may wait on each source before giving up on it.
const OVERPASS_FIRST_ANSWER: Duration = Duration::from_secs(12);
const OVERPASS_BUDGET: Duration = Duration::from_secs(45);
const TILE_BUDGET: Duration = Duration::from_secs(40);
const MAX_TILES: usize = 500;
const MAX_BUILDINGS: usize = 80_000;
/// The map changes slowly; refresh a route's buildings twice a year. Outline-only
/// data is retried sooner, in the hope of getting the detailed version next time.
const MAX_AGE_MS: u64 = 180 * 86_400_000;
const MAX_AGE_OUTLINES_MS: u64 = 14 * 86_400_000;
const FORMAT: u32 = 2;

#[derive(Serialize, Deserialize)]
struct Cached {
    format: u32,
    fetched_at: u64,
    /// False when the data came from vector tiles and so has estimated heights.
    detailed: bool,
    footprints: Vec<Footprint>,
}

pub fn cache_name(route_id: &str) -> String {
    format!("buildings-{route_id}")
}

async fn overpass_request(http: &reqwest::Client, endpoint: &'static str, query: &str) -> Result<(Vec<Footprint>, &'static str), String> {
    let res = http.post(endpoint).form(&[("data", query)]).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("{endpoint} answered {}", res.status()));
    }
    Ok((parse_overpass(&res.text().await.map_err(|e| e.to_string())?)?, endpoint))
}

async fn from_overpass(http: &reqwest::Client, route: &Route) -> Result<Vec<Footprint>, String> {
    // Diagnostic switch: go straight to the vector-tile source.
    if std::env::var_os("WM_SKIP_OVERPASS").is_some() {
        return Err("skipped by WM_SKIP_OVERPASS".into());
    }
    let per_stretch = ((STRETCH_M / route.step) as usize).max(1);
    let stretches: Vec<String> = (0..route.n).step_by(per_stretch).map(|from| overpass_query(route, from, (from + per_stretch).min(route.n), RADIUS_M, SPACING_M)).collect();

    let race = select_ok(OVERPASS.map(|endpoint| Box::pin(overpass_request(http, endpoint, &stretches[0]))));
    let ((mut all, server), _) = tokio::time::timeout(OVERPASS_FIRST_ANSWER, race).await.map_err(|_| "no Overpass server answered in time".to_string())??;
    let rest = async {
        for query in &stretches[1..] {
            all.extend(overpass_request(http, server, query).await?.0);
        }
        Ok::<_, String>(all)
    };
    let mut all = tokio::time::timeout(OVERPASS_BUDGET, rest).await.map_err(|_| "Overpass took too long".to_string())??;
    // Neighbouring stretches overlap, so the same building can come back twice.
    all.sort_by_key(|f| f.id);
    all.dedup_by_key(|f| f.id);
    Ok(all)
}

async fn from_vector_tiles(http: &reqwest::Client, route: &Route) -> Result<Vec<Footprint>, String> {
    let tiles = building_tiles(route, RADIUS_M);
    if tiles.len() > MAX_TILES {
        return Err(format!("route needs {} map tiles, more than is reasonable to download", tiles.len()));
    }
    let wanted = tiles.len();
    let fetch = |(x, y): (u32, u32)| async move {
        let res = http.get(format!("{VECTOR_TILES}/{BUILDING_ZOOM}/{x}/{y}.mvt")).send().await.ok()?;
        // An empty stretch of sea or wilderness may legitimately have no tile.
        if res.status() == reqwest::StatusCode::NOT_FOUND || res.status() == reqwest::StatusCode::NO_CONTENT {
            return Some(Vec::new());
        }
        if !res.status().is_success() {
            return None;
        }
        parse_building_tile(&res.bytes().await.ok()?, BUILDING_ZOOM, x, y)
    };
    let results: Vec<Option<Vec<Footprint>>> =
        tokio::time::timeout(TILE_BUDGET, stream::iter(tiles).map(fetch).buffer_unordered(8).collect()).await.map_err(|_| "map tiles took too long".to_string())?;
    let got = results.iter().flatten().count();
    if got < wanted {
        return Err(format!("only {got} of {wanted} map tiles arrived"));
    }
    Ok(results.into_iter().flatten().flatten().collect())
}

/// Building footprints for a route: from the cache if present and fresh,
/// otherwise downloaded. Never fails — offline simply means no (or older) buildings.
pub async fn load(http: &reqwest::Client, store: &Store, route_id: &str, route: &Route) -> Vec<Footprint> {
    let name = cache_name(route_id);
    let cached: Option<Cached> = store.get(&name).filter(|c: &Cached| c.format == FORMAT);
    if let Some(c) = cached.as_ref() {
        let max_age = if c.detailed { MAX_AGE_MS } else { MAX_AGE_OUTLINES_MS };
        if now_ms().saturating_sub(c.fetched_at) < max_age {
            return cached.unwrap().footprints;
        }
    }

    let fetched = match from_overpass(http, route).await {
        Ok(found) => Ok((found, true)),
        Err(overpass_error) => from_vector_tiles(http, route).await.map(|found| (found, false)).map_err(|tile_error| format!("Overpass: {overpass_error}; vector tiles: {tile_error}")),
    };
    match fetched {
        Ok((mut footprints, detailed)) => {
            footprints.truncate(MAX_BUILDINGS);
            eprintln!("Buildings: {} from {}", footprints.len(), if detailed { "the Overpass API" } else { "OpenStreetMap vector tiles (heights estimated)" });
            // Only a complete download is cached, so a flaky connection can't leave a route half-built.
            let entry = Cached { format: FORMAT, fetched_at: now_ms(), detailed, footprints };
            if let Err(e) = store.set(&name, &entry) {
                eprintln!("Could not cache buildings: {e}");
            }
            entry.footprints
        }
        Err(why) => {
            eprintln!("Buildings unavailable for this ride ({why})");
            cached.map(|c| c.footprints).unwrap_or_default()
        }
    }
}
