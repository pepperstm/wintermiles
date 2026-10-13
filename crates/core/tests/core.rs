use std::sync::Arc;

use wintermiles_core::fit::{encode_fit, fit_crc, FitSample, FitSession};
use wintermiles_core::gamify::{apply_ride, level_for, RideFacts};
use wintermiles_core::gpx::{build_route, demo_route, parse_gpx, RawPoint, Route, M_PER_DEG};
use wintermiles_core::health::{parse_health_date, HealthScanner};
use wintermiles_core::physics::{step_speed, PhysicsParams};
use wintermiles_core::ride::{RideEngine, RideInput, RivalKind};
use wintermiles_core::types::{BotSettings, HealthSummary, PersonalBest, Profile, Settings};
use wintermiles_core::world::{align_dem, Dem, World, CELLS, CHUNK, FAR_CELLS, TILE};

/// A straight road heading north with a given elevation profile.
fn straight(length_m: f64, ele_at: impl Fn(f64) -> Option<f64>) -> Vec<RawPoint> {
    (0..=(length_m / 10.0) as usize)
        .map(|i| {
            let d = i as f64 * 10.0;
            RawPoint { lat: 45.0 + d / M_PER_DEG, lon: 7.0, ele: ele_at(d) }
        })
        .collect()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

mod gpx {
    use super::*;

    #[test]
    fn parses_track_points_elevation_and_name() {
        let gpx = r#"<?xml version="1.0"?><gpx xmlns="http://www.topografix.com/GPX/1/1"><metadata><name>meta</name></metadata><trk><name>Col &amp; Back</name><trkseg>
          <trkpt lat="45.0" lon="7.0"><ele>100.5</ele><time>2024-01-01T00:00:00Z</time></trkpt>
          <trkpt lon='7.001' lat='45.001'><ele>110</ele></trkpt>
          <trkpt lat="45.002" lon="7.002"/>
        </trkseg></trk></gpx>"#;
        let (name, points) = parse_gpx(gpx).unwrap();
        assert_eq!(name, "Col & Back");
        assert_eq!(
            points,
            vec![
                RawPoint { lat: 45.0, lon: 7.0, ele: Some(100.5) },
                RawPoint { lat: 45.001, lon: 7.001, ele: Some(110.0) },
                RawPoint { lat: 45.002, lon: 7.002, ele: None },
            ]
        );
    }

    #[test]
    fn falls_back_to_route_points_and_rejects_bad_files() {
        let rte = r#"<gpx><rte><rtept lat="1" lon="2"/><rtept lat="1.1" lon="2"/></rte></gpx>"#;
        assert_eq!(parse_gpx(rte).unwrap().1.len(), 2);
        assert!(parse_gpx("<gpx></gpx>").is_err());
        assert!(parse_gpx("not xml at all").is_err());
    }

    #[test]
    fn resamples_to_a_fixed_step_with_correct_length_and_gradient() {
        let route = build_route("ramp", &straight(2000.0, |d| Some(100.0 + d * 0.05))).unwrap();
        assert_eq!(route.step, 5.0);
        assert!(route.total > 1990.0 && route.total <= 2001.0);
        assert!(close(route.grade[200] as f64, 0.05, 0.001));
        assert!(route.ascent > 90.0 && route.ascent < 101.0);
        let p = route.sample_at(1000.0);
        assert!(close(p.ele, 150.0, 0.5));
        // Heading north = −Z in the 3D frame.
        assert!(route.z[route.n - 1] < route.z[0] - 1900.0);
        assert!(route.x.iter().all(|x| x.abs() < 0.01));
    }

    #[test]
    fn detects_and_categorises_a_climb() {
        // 2 km flat, 5 km at 8%, 2 km flat.
        let route = build_route("col", &straight(9000.0, |d| Some(200.0 + (d - 2000.0).clamp(0.0, 5000.0) * 0.08))).unwrap();
        assert_eq!(route.climbs.len(), 1);
        let c = &route.climbs[0];
        assert!(c.start > 1900.0 && c.start < 2150.0, "start {}", c.start);
        assert!(c.end > 6900.0 && c.end < 7150.0, "end {}", c.end);
        assert!(c.gain > 385.0);
        assert_eq!(c.category, "2");
    }

    #[test]
    fn handles_routes_without_elevation() {
        let route = build_route("flat", &straight(1000.0, |_| None)).unwrap();
        assert!(!route.has_elevation);
        assert_eq!(route.ascent, 0.0);
        assert!(route.climbs.is_empty());
    }

    #[test]
    fn rounds_sharp_corners() {
        // 500 m north then a 90° turn east.
        let mut pts = straight(500.0, |_| Some(0.0));
        let corner = *pts.last().unwrap();
        pts.extend((1..=50).map(|i| RawPoint { lon: corner.lon + i as f64 * 10.0 / (M_PER_DEG * 45f64.to_radians().cos()), ..corner }));
        let route = build_route("corner", &pts).unwrap();
        // No single 5 m step turns by more than ~30°, where the raw polyline turned 90° at once.
        let mut worst = 0.0f64;
        for i in 1..route.n - 1 {
            let h1 = (route.x[i] - route.x[i - 1]).atan2(route.z[i] - route.z[i - 1]);
            let h2 = (route.x[i + 1] - route.x[i]).atan2(route.z[i + 1] - route.z[i]);
            let d = (h2 - h1).sin().atan2((h2 - h1).cos()).abs();
            worst = worst.max(d);
        }
        assert!(worst < 0.55, "worst turn {worst}");
        assert!(route.total > 985.0 && route.total < 1001.0);
    }

    #[test]
    fn builds_the_demo_loop() {
        let route = demo_route();
        assert!(route.total > 10_000.0);
        assert!(!route.climbs.is_empty());
        assert!(route.grade.iter().cloned().fold(0.0f32, f32::max) < 0.16);
    }
}

mod physics {
    use super::*;

    fn settle(power: f64, grade: f64) -> f64 {
        let p = PhysicsParams { mass: 84.0, crr: 0.004, cda: 0.32, max_speed: 25.0 };
        let mut v = 0.0;
        for _ in 0..6000 {
            v = step_speed(v, power, grade, 0.0, &p, 0.1);
        }
        v * 3.6
    }

    #[test]
    fn gives_realistic_steady_state_speeds() {
        let flat = settle(250.0, 0.0);
        assert!(flat > 33.0 && flat < 38.0, "flat {flat}");
        let climb = settle(250.0, 0.08);
        assert!(climb > 11.0 && climb < 14.0, "climb {climb}");
    }

    #[test]
    fn coasts_downhill_stalls_uphill_and_respects_the_speed_cap() {
        assert!(settle(0.0, -0.05) > 40.0);
        assert_eq!(settle(0.0, 0.05), 0.0);
        assert!(close(settle(400.0, -0.15), 90.0, 1e-6));
    }
}

mod fit {
    use super::*;

    #[test]
    fn computes_the_reference_crc() {
        // CRC-16/ARC check value.
        assert_eq!(fit_crc(b"123456789"), 0xbb3d);
    }

    #[test]
    fn writes_a_file_an_independent_decoder_reads_back() {
        let t0: u64 = 1_768_500_000_000; // 2026-01-15T18:00:00Z
        let samples: Vec<FitSample> = (0..120)
            .map(|i| FitSample {
                time: t0 + i * 1000,
                lat: 46.2 + i as f64 * 1e-5,
                lon: -7.6 - i as f64 * 1e-5,
                ele: 620.0 + i as f64 * 0.1,
                dist: i as f64 * 8.0,
                speed: 8.0,
                power: 200.0 + i as f64,
                hr: (i != 0).then_some(140.0),
                cadence: Some(88.0),
            })
            .collect();
        let session = FitSession {
            start_time: t0,
            end_time: t0 + 119_000,
            timer_s: 119.0,
            distance_m: 952.0,
            ascent_m: 12.0,
            avg_power: 259.0,
            max_power: 319.0,
            np: 262.0,
            avg_hr: 140.0,
            max_hr: 140.0,
            avg_cadence: 88.0,
            max_cadence: 88.0,
            max_speed: 8.0,
            kj: 30.8,
            ..Default::default()
        };
        let bytes = encode_fit(&samples, &session);
        let records = fitparser::from_bytes(&bytes).expect("valid FIT file");

        let field = |r: &fitparser::FitDataRecord, name: &str| -> Option<String> { r.fields().iter().find(|f| f.name() == name).map(|f| f.value().to_string()) };
        let num = |r: &fitparser::FitDataRecord, name: &str| -> f64 { field(r, name).unwrap_or_else(|| panic!("missing {name}")).parse().unwrap() };
        let of_kind = |kind: fitparser::profile::MesgNum| -> Vec<&fitparser::FitDataRecord> { records.iter().filter(|r| r.kind() == kind).collect() };

        let recs = of_kind(fitparser::profile::MesgNum::Record);
        assert_eq!(recs.len(), 120);
        let r = recs[60];
        assert_eq!(num(r, "power"), 260.0);
        assert_eq!(num(r, "cadence"), 88.0);
        assert_eq!(num(r, "heart_rate"), 140.0);
        assert!(close(num(r, "distance"), 480.0, 0.01));
        assert!(close(num(r, "enhanced_altitude").max(field(r, "altitude").and_then(|v| v.parse().ok()).unwrap_or(0.0)), 626.0, 0.3));
        // Positions come back in semicircles.
        let semi = 2_147_483_648.0 / 180.0;
        assert!(close(num(r, "position_lat") / semi, 46.2006, 1e-4));
        assert!(close(num(r, "position_long") / semi, -7.6006, 1e-4));
        assert!(field(recs[0], "heart_rate").is_none(), "missing HR must be encoded as invalid");

        let sessions = of_kind(fitparser::profile::MesgNum::Session);
        let s = sessions[0];
        assert_eq!(field(s, "sport").as_deref(), Some("cycling"));
        assert_eq!(field(s, "sub_sport").as_deref(), Some("virtual_activity"));
        assert!(close(num(s, "total_distance"), 952.0, 0.01));
        assert!(close(num(s, "total_timer_time"), 119.0, 0.01));
        assert_eq!(num(s, "avg_power"), 259.0);
        assert_eq!(num(s, "normalized_power"), 262.0);
        assert_eq!(num(s, "total_ascent"), 12.0);
        assert_eq!(of_kind(fitparser::profile::MesgNum::Lap).len(), 1);
        assert_eq!(field(of_kind(fitparser::profile::MesgNum::FileId)[0], "type").as_deref(), Some("activity"));
    }
}

mod ride {
    use super::*;

    fn settings() -> Settings {
        Settings { bots: BotSettings { enabled: true, wkg: vec![2.0] }, ..Default::default() }
    }

    fn run(engine: &mut RideEngine, seconds: u32, power: f64, clock: &mut u64) {
        for _ in 0..seconds * 10 {
            if engine.finished {
                break;
            }
            *clock += 100;
            engine.tick(0.1, RideInput { power, cadence: Some(90.0), hr: Some(150.0) }, *clock);
        }
    }

    fn flat() -> Arc<Route> {
        Arc::new(build_route("flat", &straight(3000.0, |_| Some(100.0))).unwrap())
    }

    #[test]
    fn stays_put_until_the_rider_pedals_then_records_one_sample_per_second() {
        let mut clock = 1_700_000_000_000;
        let mut engine = RideEngine::new(flat(), &settings(), None);
        run(&mut engine, 5, 0.0, &mut clock);
        assert_eq!(engine.distance, 0.0);
        assert_eq!(engine.elapsed, 0.0);
        run(&mut engine, 60, 200.0, &mut clock);
        assert_eq!(engine.samples.len(), 60);
        assert!(close(engine.elapsed, 60.0, 1e-6));
        assert!(engine.distance > 350.0);
        assert_eq!(engine.avg_power(), 200.0);
        assert!(close(engine.np(), 200.0, 1e-6));
        assert!(close(engine.kj(), 12.0, 1e-6));
        assert!(engine.rivals[0].distance > 200.0);
        assert!(engine.samples.windows(2).all(|w| w[1].time > w[0].time));
    }

    #[test]
    fn finishes_the_route_times_climbs_and_reports_the_gap_to_the_ghost() {
        let route = Arc::new(build_route("hill", &straight(3000.0, |d| Some(100.0 + (d - 1000.0).clamp(0.0, 1000.0) * 0.06))).unwrap());
        assert_eq!(route.climbs.len(), 1);
        let mut clock = 1_700_000_000_000;
        let mut first = RideEngine::new(route.clone(), &settings(), None);
        run(&mut first, 3600, 220.0, &mut clock);
        assert!(first.finished);
        assert_eq!(first.distance, route.total);
        assert!(first.climb_times[&0] > 60.0);
        assert!(first.ghost_gap().is_none());
        assert!(first.ascent > 50.0);

        let pb = PersonalBest { time_s: first.elapsed, trace: first.trace.clone(), climbs: first.climb_times.clone() };
        let mut second = RideEngine::new(route, &settings(), Some(pb));
        assert_eq!(second.rivals[0].kind, RivalKind::Ghost);
        run(&mut second, 120, 300.0, &mut clock);
        // More power than the PB ride → ahead of the ghost → negative gap.
        assert!(second.ghost_gap().unwrap() < -3.0);
        assert!(second.rivals[0].distance < second.distance);
        run(&mut second, 3600, 300.0, &mut clock);
        assert!(second.elapsed < first.elapsed);
        let result = second.last_climb_result.as_ref().unwrap();
        assert!(close(result.previous_best.unwrap(), first.climb_times[&0], 1e-9));
        assert!(result.time_s < first.climb_times[&0]);
    }

    #[test]
    fn pauses_on_request() {
        let mut clock = 0;
        let mut engine = RideEngine::new(flat(), &settings(), None);
        run(&mut engine, 10, 200.0, &mut clock);
        let d = engine.distance;
        engine.user_paused = true;
        run(&mut engine, 10, 200.0, &mut clock);
        assert_eq!(engine.distance, d);
        assert_eq!(engine.trainer_grade(1.0), 0.0);
    }
}

mod gamify {
    use super::*;

    #[test]
    fn awards_xp_levels_and_badges_once() {
        let ride = RideFacts { distance_m: 40_000.0, ascent_m: 600.0, duration_s: 5400.0, completed: true, beat_pb: false, np: 150.0, ftp: 200.0, climb_categories: vec!["3".into()] };
        let mut profile = Profile::default();
        let (xp, mut badges) = apply_ride(&mut profile, &ride);
        assert_eq!(xp, 400 + 120 + 50);
        badges.sort();
        assert_eq!(badges, ["climber", "finisher", "first-ride", "hour"]);
        let (_, again) = apply_ride(&mut profile, &ride);
        assert!(again.is_empty());
        assert_eq!(profile.rides, 2);
        assert_eq!(level_for(0).0, 1);
        assert_eq!(level_for(200).0, 2);
        assert_eq!(level_for(799), (2, 599, 600));
    }
}

mod health {
    use super::*;

    #[test]
    fn parses_export_dates_with_a_timezone() {
        // 2026-01-05T07:12:00Z
        assert_eq!(parse_health_date("2026-01-05 08:12:00 +0100"), Some(1_767_597_120_000));
        assert_eq!(parse_health_date("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(parse_health_date("garbage"), None);
    }

    #[test]
    fn extracts_the_latest_body_metrics_across_chunk_boundaries() {
        let now = 1_790_812_800_000; // 2026-10-01
        let xml = [
            r#"<HealthData locale="en_GB">"#,
            r#" <Me HKCharacteristicTypeIdentifierDateOfBirth="1984-03-02" HKCharacteristicTypeIdentifierBiologicalSex="HKBiologicalSexMale"/>"#,
            r#" <Record type="HKQuantityTypeIdentifierBodyMass" sourceName="Scale" unit="kg" creationDate="2026-09-01 07:00:00 +0000" startDate="2026-09-01 07:00:00 +0000" endDate="2026-09-01 07:00:00 +0000" value="74.26"/>"#,
            r#" <Record type="HKQuantityTypeIdentifierBodyMass" sourceName="Scale" unit="lb" creationDate="2025-01-01 07:00:00 +0000" startDate="2025-01-01 07:00:00 +0000" endDate="2025-01-01 07:00:00 +0000" value="180"/>"#,
            r#" <Record type="HKQuantityTypeIdentifierHeartRate" sourceName="Watch" unit="count/min" startDate="2026-08-01 10:00:00 +0000" endDate="2026-08-01 10:00:00 +0000" value="181">"#,
            r#"  <MetadataEntry key="HKMetadataKeyHeartRateMotionContext" value="2"/>"#,
            r#" </Record>"#,
            r#" <Record type="HKQuantityTypeIdentifierHeartRate" sourceName="Watch" unit="count/min" startDate="2023-08-01 10:00:00 +0000" endDate="2023-08-01 10:00:00 +0000" value="195"/>"#,
            r#" <Record type="HKQuantityTypeIdentifierRestingHeartRate" sourceName="Watch" unit="count/min" startDate="2026-09-20 00:00:00 +0000" endDate="2026-09-20 00:00:00 +0000" value="52"/>"#,
            r#" <Record type="HKQuantityTypeIdentifierVO2Max" sourceName="Watch" unit="mL/min·kg" startDate="2026-09-20 00:00:00 +0000" endDate="2026-09-20 00:00:00 +0000" value="48.31"/>"#,
            r#" <Record type="HKQuantityTypeIdentifierCyclingFunctionalThresholdPower" sourceName="Watch" unit="W" startDate="2026-09-20 00:00:00 +0000" endDate="2026-09-20 00:00:00 +0000" value="238"/>"#,
            r#" <Workout workoutActivityType="HKWorkoutActivityTypeCycling" duration="60"/>"#,
            r#" <Workout workoutActivityType="HKWorkoutActivityTypeRunning" duration="30"/>"#,
            r#"</HealthData>"#,
        ]
        .join("\n");
        let mut scanner = HealthScanner::new(now);
        // 37-byte chunks split lines and the multi-byte "·" alike.
        for chunk in xml.as_bytes().chunks(37) {
            scanner.push(chunk);
        }
        assert_eq!(
            scanner.finish(),
            HealthSummary {
                weight_kg: Some(74.3),
                weight_date: Some("2026-09-01".into()),
                resting_hr: Some(52),
                max_hr_observed: Some(181),
                vo2max: Some(48.3),
                ftp: Some(238),
                date_of_birth: Some("1984-03-02".into()),
                cycling_workouts: 1,
                records: 7,
            }
        );
    }
}

mod world {
    use super::*;

    #[test]
    fn terrain_is_pinned_to_the_road_and_meshes_have_the_expected_shape() {
        let route = Arc::new(demo_route());
        let world = World::new(route.clone(), None, 0.0);
        // Just below road level on the centreline, all along the route.
        for i in (0..route.n).step_by(97) {
            let (h, d) = world.field.height(route.x[i], route.z[i]);
            assert!(d < 6.0);
            let below = route.ele[i] as f64 - h;
            assert!(below > 0.1 && below < 0.9, "sample {i}: road {} ground {h}", route.ele[i]);
        }
        let row = CELLS + 1;
        let cx = (route.x[0] / CHUNK).floor() as i32;
        let cz = (route.z[0] / CHUNK).floor() as i32;
        let chunk = world.chunk(cx, cz);
        assert_eq!(chunk.positions.len(), row * row * 3);
        assert_eq!(chunk.normals.len(), row * row * 3);
        assert_eq!(chunk.colors.len(), row * row * 3);
        assert_eq!(chunk.trees.len() % 6, 0);
        assert!(chunk.positions.iter().chain(&chunk.normals).chain(&chunk.colors).all(|v| v.is_finite()));
        // Neighbouring chunks share their edge exactly, so there are no cracks.
        let next = world.chunk(cx + 1, cz);
        for j in 0..row {
            let a = (j * row + CELLS) * 3;
            let b = (j * row) * 3;
            assert_eq!(chunk.positions[a + 1], next.positions[b + 1]);
            assert_eq!(chunk.normals[a + 1], next.normals[b + 1]);
        }
        let far = world.far(route.x[0], route.z[0]);
        assert_eq!(far.positions.len(), (FAR_CELLS + 1).pow(2) * 3);
        let road = world.road();
        assert_eq!(road.positions.len(), route.n * 18);
        assert_eq!(road.uvs.len(), route.n * 12);
    }

    #[test]
    fn elevation_is_filled_in_from_terrain_tiles() {
        // A route with no elevation over a synthetic tile set that slopes up to the north.
        let mut route = build_route("planner", &straight(3000.0, |_| None)).unwrap();
        let (zoom, keys) = Dem::plan(&route).unwrap();
        assert!(!keys.is_empty() && keys.len() <= 64);
        let mut dem = Dem::new(zoom);
        for key in &keys {
            let mut px = vec![0u8; TILE * TILE * 3];
            for y in 0..TILE {
                // Height rises with latitude: 1 m per pixel row going north (decreasing y).
                let h = 2000.0 - (key.1 as f64 * TILE as f64 + y as f64 - keys[0].1 as f64 * TILE as f64);
                let v = (h + 32768.0) as u32;
                for x in 0..TILE {
                    let o = (y * TILE + x) * 3;
                    px[o] = (v / 256) as u8;
                    px[o + 1] = (v % 256) as u8;
                }
            }
            assert!(dem.insert_rgb(*key, &px, 3));
        }
        let offset = align_dem(&mut route, &dem);
        assert_eq!(offset, 0.0);
        assert!(route.has_elevation);
        assert!(route.ele[route.n - 1] > route.ele[0] + 20.0, "{} → {}", route.ele[0], route.ele[route.n - 1]);
        assert!(route.grade.iter().all(|g| *g > -0.01));
        assert!(route.ascent > 20.0);
    }
}

#[test]
fn hairpin_ground_never_buries_the_lower_arm() {
    // Up a 10% ramp heading north, a tight U-turn, and back south 8 m to the east:
    // two arms of road side by side, climbing in opposite directions.
    let m_lon = M_PER_DEG * 45f64.to_radians().cos();
    let mut pts: Vec<RawPoint> = (0..=40).map(|i| RawPoint { lat: 45.0 + i as f64 * 5.0 / M_PER_DEG, lon: 7.0, ele: Some(i as f64 * 0.5) }).collect();
    pts.extend((1..=40).map(|i| RawPoint { lat: 45.0 + (200.0 - i as f64 * 5.0) / M_PER_DEG, lon: 7.0 + 8.0 / m_lon, ele: Some(20.0 + i as f64 * 0.5) }));
    let route = Arc::new(build_route("hairpin", &pts).unwrap());
    let world = World::new(route.clone(), None, 0.0);
    // Everywhere on the tarmac of either arm, the ground must be below the road surface.
    // (The U-turn itself is excluded: with a 4 m radius its inside edge is one patch
    // of tarmac shared by both directions.)
    let apex = route.n / 2;
    for i in (0..route.n).step_by(3).filter(|i| i.abs_diff(apex) > 4) {
        for side in [-3.0, 0.0, 3.0] {
            let (h, _) = world.field.height(route.x[i] + side, route.z[i]);
            assert!(h < route.ele[i] as f64, "sample {i} side {side}: ground {h} above road {}", route.ele[i]);
        }
    }
}

mod buildings {
    use super::*;
    use wintermiles_core::world::{overpass_query, parse_overpass, Footprint};

    #[test]
    fn parses_overpass_ways_relations_and_heights() {
        let json = r#"{"version":0.6,"elements":[
          {"type":"way","id":1,"tags":{"building":"house"},"geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0001,"lon":7.0},{"lat":45.0001,"lon":7.0001},{"lat":45.0,"lon":7.0}]},
          {"type":"way","id":2,"tags":{"building":"yes","height":"12.5 m"},"geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0001,"lon":7.0},{"lat":45.0001,"lon":7.0001},{"lat":45.0,"lon":7.0}]},
          {"type":"way","id":3,"tags":{"building":"yes","building:levels":"4"},"geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0001,"lon":7.0},{"lat":45.0001,"lon":7.0001},{"lat":45.0,"lon":7.0}]},
          {"type":"way","id":4,"tags":{"building":"roof"},"geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0001,"lon":7.0},{"lat":45.0001,"lon":7.0001},{"lat":45.0,"lon":7.0}]},
          {"type":"way","id":5,"tags":{"building":"yes"},"geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0001,"lon":7.0},{"lat":45.0001,"lon":7.0001}]},
          {"type":"way","id":6,"tags":{"building":"yes"},"geometry":[{"lat":45.0,"lon":7.0},null,{"lat":45.0001,"lon":7.0001},{"lat":45.0,"lon":7.0}]},
          {"type":"relation","id":7,"tags":{"building":"church","type":"multipolygon"},"members":[
            {"type":"way","ref":70,"role":"outer","geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0002,"lon":7.0},{"lat":45.0002,"lon":7.0002},{"lat":45.0,"lon":7.0}]},
            {"type":"way","ref":71,"role":"inner","geometry":[{"lat":45.0,"lon":7.0},{"lat":45.0001,"lon":7.0},{"lat":45.0001,"lon":7.0001},{"lat":45.0,"lon":7.0}]}]}
        ]}"#;
        let found = parse_overpass(json).unwrap();
        // The canopy, the open outline and the outline with a missing node are skipped.
        assert_eq!(found.iter().map(|f| f.height).collect::<Vec<_>>(), [6.5, 12.5, 13.0, 14.0]);
        assert_eq!(found[0].ring.len(), 4);
        assert!(parse_overpass("<html>rate limited</html>").is_err());
    }

    #[test]
    fn builds_a_query_along_the_route() {
        let route = build_route("flat", &straight(1000.0, |_| Some(100.0))).unwrap();
        let q = overpass_query(&route, 0, route.n, 350.0, 100.0);
        assert!(q.starts_with("[out:json][timeout:25];(way[\"building\"](around:350,45.00000,7.00000,"));
        assert!(q.ends_with("out geom;"));
        // ~11 points at 100 m spacing plus the end point, used once for ways and once for relations.
        assert_eq!(q.matches("7.00000").count(), 24);
    }

    /// A rectangle (or any outline) given in local metres, as an OSM-style footprint.
    fn footprint(route: &Route, id: u64, height: f32, outline: &[(f64, f64)]) -> Footprint {
        let mut ring: Vec<[f64; 2]> = outline.iter().map(|(x, z)| route.to_lat_lon(*x, *z)).map(|(lat, lon)| [lat, lon]).collect();
        ring.push(ring[0]);
        Footprint { id, height, ring }
    }

    #[test]
    fn extrudes_buildings_onto_the_terrain_and_keeps_the_road_clear() {
        let route = Arc::new(build_route("flat", &straight(1000.0, |_| Some(100.0))).unwrap());
        // The road runs along x = 0. One house 30 m to the side, an L-shaped barn 60 m away, and one mapped across the road.
        let (z0, _) = (route.z[100], ());
        let house = [(30.0, z0), (40.0, z0), (40.0, z0 + 8.0), (30.0, z0 + 8.0)];
        let barn = [(60.0, z0), (80.0, z0), (80.0, z0 + 10.0), (70.0, z0 + 10.0), (70.0, z0 + 20.0), (60.0, z0 + 20.0)];
        let on_road = [(-4.0, z0 + 40.0), (6.0, z0 + 40.0), (6.0, z0 + 50.0), (-4.0, z0 + 50.0)];
        let prints = [footprint(&route, 1, 6.0, &house), footprint(&route, 2, 8.0, &barn), footprint(&route, 3, 6.0, &on_road)];
        let world = World::new(route.clone(), None, 0.0).with_buildings(&prints);
        assert_eq!(world.buildings.count(), 2);
        assert!(world.buildings.covers(35.0, z0 + 4.0));
        assert!(!world.buildings.covers(75.0, z0 + 15.0), "the notch of the L is outside");
        assert!(!world.buildings.covers(0.0, z0 + 45.0));

        let (cx, cz) = ((35.0 / CHUNK).floor() as i32, ((z0 + 4.0) / CHUNK).floor() as i32);
        let mesh = world.chunk(cx, cz).buildings;
        // House: 4 walls × 2 triangles, a pitched roof of 4 and 2 gable ends. Barn: 6 walls × 2,
        // plus its L-shaped flat roof in 3 or 4 triangles depending on how it is cut up.
        assert!([29 * 9, 30 * 9].contains(&mesh.positions.len()), "{} floats", mesh.positions.len());
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.colors.len(), mesh.positions.len());
        assert_eq!(mesh.uvs.len(), mesh.positions.len() / 3 * 2);
        assert!(mesh.positions.iter().chain(&mesh.colors).chain(&mesh.uvs).all(|v| v.is_finite()));
        // The house's 10 m walls carry three window bays; no wall has a fraction of one.
        assert!(mesh.uvs.chunks(2).any(|uv| uv[0] == 3.0) && mesh.uvs.chunks(2).all(|uv| uv[0] == 0.04 || uv[0].fract() == 0.0));

        let tri = |t: usize| -> [[f64; 3]; 3] { [0, 1, 2].map(|k| [0, 1, 2].map(|c| mesh.positions[t + k * 3 + c] as f64)) };
        let mut flat_roof_area = 0.0f64;
        let mut ridge = f64::MIN;
        let mut walls_facing_out = 0;
        for t in (0..mesh.positions.len()).step_by(9) {
            let p = tri(t);
            let (u, v) = ([p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]], [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]]);
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let stored = [mesh.normals[t] as f64, mesh.normals[t + 1] as f64, mesh.normals[t + 2] as f64];
            // The stored normal agrees with the winding, so the renderer lights the outside.
            assert!(n[0] * stored[0] + n[1] * stored[1] + n[2] * stored[2] > 0.0, "triangle {} {p:?}: winding normal {n:?}, stored {stored:?}", t / 9);
            // No face of a building looks downward.
            assert!(stored[1] >= -1e-6);
            if stored[1] > 0.999 {
                flat_roof_area += (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() / 2.0;
            } else if stored[1].abs() < 1e-6 {
                // A wall: its normal must point away from the middle of its own building.
                let mid = [(p[0][0] + p[1][0] + p[2][0]) / 3.0, (p[0][2] + p[1][2] + p[2][2]) / 3.0];
                let centre = if mid[0] < 50.0 { (35.0, z0 + 4.0) } else { (66.0, z0 + 7.0) };
                if (mid[0] - centre.0) * stored[0] + (mid[1] - centre.1) * stored[2] > 0.0 {
                    walls_facing_out += 1;
                }
            }
            ridge = ridge.max(p.iter().map(|q| q[1]).fold(f64::MIN, f64::max));
        }
        // Only the barn has a flat roof (300 m²); the house's is pitched.
        assert!(close(flat_roof_area, 300.0, 0.5), "flat roof area {flat_roof_area}");
        // All 20 wall triangles of the two buildings, plus the house's two gable ends.
        assert!(walls_facing_out >= 21, "{walls_facing_out} wall faces point outward");

        // Eaves sit the building's height above the average ground at its corners
        // (or just clear of the highest corner on a steep slope).
        let eaves_for = |outline: &[(f64, f64)], height: f64| {
            let g: Vec<f64> = outline.iter().map(|p| world.field.height(p.0, p.1).0).collect();
            (g.iter().sum::<f64>() / g.len() as f64 + height).max(g.iter().cloned().fold(f64::MIN, f64::max) + 2.5)
        };
        // The house is 10 × 8 m, so its ridge rises 0.32 × 8 m above the eaves.
        let expected = (eaves_for(&house, 6.0) + 2.56).max(eaves_for(&barn, 8.0));
        assert!(close(ridge, expected, 0.01), "highest point {ridge} expected {expected}");
        // No tree grows through a building.
        let trees = world.chunk(cx, cz).trees;
        assert!(trees.chunks(6).all(|t| !world.buildings.covers(t[0] as f64, t[2] as f64)));
    }
}

mod vector_tiles {
    use super::*;
    use wintermiles_core::world::{building_tiles, parse_building_tile, BUILDING_ZOOM};

    // A tiny protobuf writer, enough to assemble a vector tile by hand.
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn field_varint(field: u64, v: u64, out: &mut Vec<u8>) {
        varint(field << 3, out);
        varint(v, out);
    }
    fn field_bytes(field: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint(field << 3 | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }
    fn zig(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }
    /// A polygon feature from rings given as absolute tile coordinates.
    fn polygon(rings: &[&[(i64, i64)]]) -> Vec<u8> {
        let mut geometry = Vec::new();
        let (mut x, mut y) = (0, 0);
        for ring in rings {
            varint(1 << 3 | 1, &mut geometry); // MoveTo ×1
            varint(zig(ring[0].0 - x), &mut geometry);
            varint(zig(ring[0].1 - y), &mut geometry);
            (x, y) = ring[0];
            varint(((ring.len() - 1) as u64) << 3 | 2, &mut geometry); // LineTo ×(n−1)
            for p in &ring[1..] {
                varint(zig(p.0 - x), &mut geometry);
                varint(zig(p.1 - y), &mut geometry);
                (x, y) = *p;
            }
            varint(1 << 3 | 7, &mut geometry); // ClosePath
        }
        let mut feature = Vec::new();
        field_varint(1, 99, &mut feature);
        field_varint(3, 3, &mut feature);
        field_bytes(4, &geometry, &mut feature);
        feature
    }
    fn layer(name: &str, features: &[Vec<u8>]) -> Vec<u8> {
        let mut layer = Vec::new();
        field_varint(15, 2, &mut layer);
        field_bytes(1, name.as_bytes(), &mut layer);
        features.iter().for_each(|f| field_bytes(2, f, &mut layer));
        field_varint(5, 4096, &mut layer);
        let mut tile = Vec::new();
        field_bytes(3, &layer, &mut tile);
        tile
    }

    #[test]
    fn decodes_building_outlines_from_a_tile() {
        // Outer rings run clockwise on screen (y down), which is positive area in tile units.
        let house: &[(i64, i64)] = &[(1000, 1000), (1100, 1000), (1100, 1080), (1000, 1080)];
        let courtyard: &[(i64, i64)] = &[(1020, 1020), (1020, 1060), (1080, 1060), (1080, 1020)];
        let in_buffer: &[(i64, i64)] = &[(-60, 500), (-10, 500), (-10, 550), (-60, 550)];
        let mut tile = layer("streets", &[polygon(&[house])]);
        tile.extend(layer("buildings", &[polygon(&[house, courtyard]), polygon(&[in_buffer])]));

        let (z, x, y) = (BUILDING_ZOOM, 8466, 5890);
        let found = parse_building_tile(&tile, z, x, y).unwrap();
        // Only the "buildings" layer counts; the courtyard and the neighbour's building are dropped.
        assert_eq!(found.len(), 1);
        let ring = &found[0].ring;
        assert_eq!(ring.len(), 5);
        assert_eq!(ring[0], ring[4]);

        // Corner (1000, 1000) of tile (x, y): check against the Web Mercator formulas directly.
        let n = 2f64.powi(z as i32);
        let lon = (x as f64 + 1000.0 / 4096.0) / n * 360.0 - 180.0;
        let lat = (std::f64::consts::PI * (1.0 - 2.0 * (y as f64 + 1000.0 / 4096.0) / n)).sinh().atan().to_degrees();
        assert!(close(ring[0][0], lat, 1e-9) && close(ring[0][1], lon, 1e-9));
        assert!(lat > 44.0 && lat < 47.0 && lon > 5.0 && lon < 7.0, "tile is in the Alps: {lat}, {lon}");
        // 100 × 80 tile units is roughly 42 × 34 m here: a large house or small block.
        assert!(found[0].height > 6.0 && found[0].height < 12.0, "height {}", found[0].height);

        assert!(parse_building_tile(&[0xff, 0xff, 0xff], z, x, y).is_none(), "garbage is rejected, not misread");
        assert_eq!(parse_building_tile(&[], z, x, y), Some(vec![]));
    }

    #[test]
    fn lists_the_tiles_along_a_route() {
        let route = build_route("flat", &straight(3000.0, |_| Some(100.0))).unwrap();
        let tiles = building_tiles(&route, 350.0);
        // 3 km of road plus margins spans only a handful of ~1.7 km tiles, each listed once.
        assert!((2..=9).contains(&tiles.len()), "{} tiles", tiles.len());
        let mut unique = tiles.clone();
        unique.dedup();
        assert_eq!(unique.len(), tiles.len());
        // Every point of the route falls in one of them.
        let n = 2f64.powi(BUILDING_ZOOM as i32);
        for i in (0..route.n).step_by(50) {
            let rad = route.lat[i].to_radians();
            let key = (((route.lon[i] + 180.0) / 360.0 * n) as u32, ((1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n) as u32);
            assert!(tiles.contains(&key));
        }
    }
}
