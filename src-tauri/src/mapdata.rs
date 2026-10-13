//! Fetches what OpenStreetMap knows about a route's surroundings and caches it,
//! so each route is normally downloaded only once.
//!
//! Scenery — forests, water, roads, bridges and tunnels — comes from OpenStreetMap's own
//! vector tiles, which are fast and dependable. Buildings have two sources, in
//! order of preference:
//! 1. The Overpass API — full detail (mapped heights, storeys, building types),
//!    but served by volunteer-run machines that are often overloaded.
//! 2. The same vector tiles — outlines only.

use std::time::Duration;

use futures::future::select_ok;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use wintermiles_core::gpx::Route;
use wintermiles_core::world::{corridor_tiles, overpass_query, parse_overpass, parse_tile, route_corridor, Footprint, MapTile, RoadKind, Scenery, MAP_ZOOM};

use crate::now_ms;
use crate::store::Store;

/// Public Overpass servers. The first stretch of a route is asked of all of them at once and the
/// first good answer wins; the rest of the route then goes to that server alone.
const OVERPASS: [&str; 3] = ["https://overpass-api.de/api/interpreter", "https://overpass.openstreetmap.fr/api/interpreter", "https://overpass.private.coffee/api/interpreter"];
const VECTOR_TILES: &str = "https://vector.openstreetmap.org/shortbread_v1";
/// Buildings are fetched this far to either side of the road.
const RADIUS_M: f64 = 350.0;
/// Forests and water are wanted further out: they are what the middle distance is made of.
const SCENERY_RADIUS_M: f64 = 1000.0;
/// Roads are kept out to about the same distance, and footpaths — of which a town has thousands —
/// only where they can be made out. (The test used errs on the generous side, up to double.)
const ROADS_RADIUS_M: f64 = 700.0;
const PATHS_RADIUS_M: f64 = 250.0;
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
const SCENERY_FORMAT: u32 = 3;

#[derive(Serialize, Deserialize)]
struct Cached {
    format: u32,
    fetched_at: u64,
    /// False when the data came from vector tiles and so has estimated heights.
    detailed: bool,
    footprints: Vec<Footprint>,
}

#[derive(Serialize, Deserialize)]
struct CachedScenery {
    format: u32,
    fetched_at: u64,
    scenery: Scenery,
}

/// What the map holds for a route. Either part is empty if it wasn't wanted or couldn't be had.
#[derive(Default)]
pub struct MapData {
    pub footprints: Vec<Footprint>,
    pub scenery: Option<Scenery>,
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

/// Downloads and decodes the vector tiles within `radius_m` of the route. All or nothing:
/// a flaky connection must not leave a route with holes in it.
async fn fetch_tiles(http: &reqwest::Client, route: &Route, radius_m: f64) -> Result<Vec<((u32, u32), MapTile)>, String> {
    let mut tiles = corridor_tiles(route, radius_m);
    // A very long route gets a narrower strip rather than nothing.
    if tiles.len() > MAX_TILES && radius_m > RADIUS_M {
        tiles = corridor_tiles(route, RADIUS_M);
    }
    if tiles.len() > MAX_TILES {
        return Err(format!("route needs {} map tiles, more than is reasonable to download", tiles.len()));
    }
    let wanted = tiles.len();
    let fetch = |(x, y): (u32, u32)| async move {
        let res = http.get(format!("{VECTOR_TILES}/{MAP_ZOOM}/{x}/{y}.mvt")).send().await.ok()?;
        // An empty stretch of sea or wilderness may legitimately have no tile.
        if res.status() == reqwest::StatusCode::NOT_FOUND || res.status() == reqwest::StatusCode::NO_CONTENT {
            return Some(((x, y), MapTile::default()));
        }
        if !res.status().is_success() {
            return None;
        }
        Some(((x, y), parse_tile(&res.bytes().await.ok()?, MAP_ZOOM, x, y)?))
    };
    let results: Vec<Option<((u32, u32), MapTile)>> =
        tokio::time::timeout(TILE_BUDGET, stream::iter(tiles).map(fetch).buffer_unordered(8).collect()).await.map_err(|_| "map tiles took too long".to_string())?;
    let got: Vec<_> = results.into_iter().flatten().collect();
    if got.len() < wanted {
        return Err(format!("only {} of {wanted} map tiles arrived", got.len()));
    }
    Ok(got)
}

fn fresh<T>(cached: &Option<T>, fetched_at: impl Fn(&T) -> u64, max_age: impl Fn(&T) -> u64) -> bool {
    cached.as_ref().is_some_and(|c| now_ms().saturating_sub(fetched_at(c)) < max_age(c))
}

/// Buildings and scenery for a route: from the cache where present and fresh, otherwise
/// downloaded. Never fails — offline simply means none (or older ones).
pub async fn load(http: &reqwest::Client, store: &Store, route_id: &str, route: &Route, want_buildings: bool, want_scenery: bool) -> MapData {
    let (buildings_name, scenery_name) = (format!("buildings-{route_id}"), format!("scenery-{route_id}"));
    let cached: Option<Cached> = if want_buildings { store.get(&buildings_name).filter(|c: &Cached| c.format == FORMAT) } else { None };
    let cached_scenery: Option<CachedScenery> = if want_scenery { store.get(&scenery_name).filter(|c: &CachedScenery| c.format == SCENERY_FORMAT) } else { None };
    let need_buildings = want_buildings && !fresh(&cached, |c| c.fetched_at, |c| if c.detailed { MAX_AGE_MS } else { MAX_AGE_OUTLINES_MS });
    let need_scenery = want_scenery && !fresh(&cached_scenery, |c| c.fetched_at, |_| MAX_AGE_MS);

    // The two sources are different servers; ask both at once.
    let (overpass, tiles) = tokio::join!(
        async {
            if need_buildings {
                Some(from_overpass(http, route).await)
            } else {
                None
            }
        },
        async {
            if need_scenery {
                Some(fetch_tiles(http, route, SCENERY_RADIUS_M).await)
            } else {
                None
            }
        }
    );

    let mut tile_buildings: Option<Vec<Footprint>> = None;
    let scenery = match tiles {
        Some(Ok(mut tiles)) => {
            let mut scenery = Scenery::default();
            let mut footprints = Vec::new();
            let (near_roads, near_paths) = (route_corridor(route, ROADS_RADIUS_M), route_corridor(route, PATHS_RADIUS_M));
            for ((x, y), tile) in &mut tiles {
                tile.roads.retain(|road| {
                    let middle = road.line[road.line.len() / 2];
                    if matches!(road.kind, RoadKind::Path | RoadKind::Footway) { near_paths(middle[0], middle[1]) } else { near_roads(middle[0], middle[1]) }
                });
                footprints.append(&mut tile.footprints);
                scenery.add(*x, *y, tile);
            }
            tile_buildings = Some(footprints);
            eprintln!(
                "Scenery: {} areas, {} waterways, {} roads and paths, {} bridges and tunnels from {} map tiles",
                scenery.areas.len(),
                scenery.waterways.len(),
                scenery.roads.len(),
                scenery.crossings.len(),
                scenery.tiles.len()
            );
            let entry = CachedScenery { format: SCENERY_FORMAT, fetched_at: now_ms(), scenery };
            if let Err(e) = store.set(&scenery_name, &entry) {
                eprintln!("Could not cache scenery: {e}");
            }
            Some(entry.scenery)
        }
        Some(Err(why)) => {
            eprintln!("Scenery unavailable for this ride ({why})");
            cached_scenery.map(|c| c.scenery)
        }
        None => cached_scenery.map(|c| c.scenery),
    };

    let footprints = match overpass {
        None => cached.map(|c| c.footprints).unwrap_or_default(),
        Some(result) => {
            let fetched = match result {
                Ok(found) => Ok((found, true)),
                // Fall back on the tiles: the ones just fetched for scenery if there are any.
                Err(overpass_error) => match tile_buildings {
                    Some(found) => {
                        let near = route_corridor(route, RADIUS_M);
                        Ok((found.into_iter().filter(|f| f.ring.first().is_some_and(|p| near(p[0], p[1]))).collect(), false))
                    }
                    None => fetch_tiles(http, route, RADIUS_M)
                        .await
                        .map(|tiles| (tiles.into_iter().flat_map(|(_, t)| t.footprints).collect(), false))
                        .map_err(|tile_error| format!("Overpass: {overpass_error}; vector tiles: {tile_error}")),
                },
            };
            match fetched {
                Ok((mut footprints, detailed)) => {
                    footprints.truncate(MAX_BUILDINGS);
                    eprintln!("Buildings: {} from {}", footprints.len(), if detailed { "the Overpass API" } else { "OpenStreetMap vector tiles (heights estimated)" });
                    // Only a complete download is cached, so a flaky connection can't leave a route half-built.
                    let entry = Cached { format: FORMAT, fetched_at: now_ms(), detailed, footprints };
                    if let Err(e) = store.set(&buildings_name, &entry) {
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
    };
    MapData { footprints, scenery }
}
