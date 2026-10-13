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
    use wintermiles_core::world::{corridor_tiles, parse_building_tile, parse_tile, AreaKind, RoadKind, WaterwayKind, MAP_ZOOM};

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
        feature(3, &geometry, &[])
    }
    /// A line feature through the given absolute tile coordinates.
    fn line(points: &[(i64, i64)], tags: &[u64]) -> Vec<u8> {
        let mut geometry = Vec::new();
        varint(1 << 3 | 1, &mut geometry);
        varint(zig(points[0].0), &mut geometry);
        varint(zig(points[0].1), &mut geometry);
        varint(((points.len() - 1) as u64) << 3 | 2, &mut geometry);
        for w in points.windows(2) {
            varint(zig(w[1].0 - w[0].0), &mut geometry);
            varint(zig(w[1].1 - w[0].1), &mut geometry);
        }
        feature(2, &geometry, tags)
    }
    /// `tags` are alternating indices into the layer's keys and values.
    fn feature(kind: u64, geometry: &[u8], tags: &[u64]) -> Vec<u8> {
        let mut feature = Vec::new();
        field_varint(1, 99, &mut feature);
        let mut packed = Vec::new();
        tags.iter().for_each(|t| varint(*t, &mut packed));
        field_bytes(2, &packed, &mut feature);
        field_varint(3, kind, &mut feature);
        field_bytes(4, geometry, &mut feature);
        feature
    }
    fn tagged(mut feature: Vec<u8>, tags: &[u64]) -> Vec<u8> {
        let mut packed = Vec::new();
        tags.iter().for_each(|t| varint(*t, &mut packed));
        field_bytes(2, &packed, &mut feature);
        feature
    }
    fn layer(name: &str, features: &[Vec<u8>]) -> Vec<u8> {
        layer_with(name, &[], &[], features)
    }
    /// A layer with attribute tables. Values are text, or `true`/`false` for flags.
    fn layer_with(name: &str, keys: &[&str], values: &[&str], features: &[Vec<u8>]) -> Vec<u8> {
        let mut layer = Vec::new();
        field_varint(15, 2, &mut layer);
        field_bytes(1, name.as_bytes(), &mut layer);
        features.iter().for_each(|f| field_bytes(2, f, &mut layer));
        keys.iter().for_each(|k| field_bytes(3, k.as_bytes(), &mut layer));
        for v in values {
            let mut value = Vec::new();
            match *v {
                "true" | "false" => field_varint(7, (*v == "true") as u64, &mut value),
                text => field_bytes(1, text.as_bytes(), &mut value),
            }
            field_bytes(4, &value, &mut layer);
        }
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

        let (z, x, y) = (MAP_ZOOM, 8466, 5890);
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
        assert!(close(ring[0][0], lat, 1e-7) && close(ring[0][1], lon, 1e-7));
        assert!(lat > 44.0 && lat < 47.0 && lon > 5.0 && lon < 7.0, "tile is in the Alps: {lat}, {lon}");
        // 100 × 80 tile units is roughly 42 × 34 m here: a large house or small block.
        assert!(found[0].height > 6.0 && found[0].height < 12.0, "height {}", found[0].height);

        assert!(parse_building_tile(&[0xff, 0xff, 0xff], z, x, y).is_none(), "garbage is rejected, not misread");
        assert_eq!(parse_building_tile(&[], z, x, y), Some(vec![]));
    }

    #[test]
    fn decodes_forests_water_bridges_and_tunnels_from_a_tile() {
        let square: &[(i64, i64)] = &[(1000, 1000), (1400, 1000), (1400, 1400), (1000, 1400)];
        let clearing: &[(i64, i64)] = &[(1100, 1100), (1100, 1200), (1200, 1200), (1200, 1100)];
        // land: keys [kind], values [forest, meadow]
        let mut tile = layer_with("land", &["kind"], &["forest", "meadow"], &[tagged(polygon(&[square, clearing]), &[0, 0]), tagged(polygon(&[square]), &[0, 1])]);
        tile.extend(layer_with("water_polygons", &["kind"], &["water"], &[tagged(polygon(&[square]), &[0, 0])]));
        // water_lines: keys [kind, tunnel], values [stream, ditch, true, false]
        let brook: &[(i64, i64)] = &[(100, 100), (300, 100), (300, 300)];
        tile.extend(layer_with("water_lines", &["kind", "tunnel"], &["stream", "ditch", "true", "false"], &[line(brook, &[0, 0, 1, 3]), line(brook, &[0, 1, 1, 3]), line(brook, &[0, 0, 1, 2])]));
        // streets: keys [kind, bridge, tunnel, rail], values [primary, rail, true, false]
        let across: &[(i64, i64)] = &[(3900, 2000), (4300, 2000)];
        let (bridge, tunnel, plain, railway) = ([0, 0, 1, 2, 2, 3, 3, 3], [0, 0, 1, 3, 2, 2, 3, 3], [0, 0, 1, 3, 2, 3, 3, 3], [0, 1, 1, 2, 2, 3, 3, 2]);
        tile.extend(layer_with("streets", &["kind", "bridge", "tunnel", "rail"], &["primary", "rail", "true", "false"], &[line(across, &bridge), line(brook, &tunnel), line(brook, &plain), line(brook, &railway)]));

        let found = parse_tile(&tile, MAP_ZOOM, 8466, 5890).unwrap();
        assert!(found.footprints.is_empty());
        // Every sort of way is something to draw, slip roads and all; a runway is not.
        let kinds = ["motorway", "trunk_link", "secondary", "residential", "service", "pedestrian", "track", "cycleway", "path", "tram", "runway"];
        let ways: Vec<Vec<u8>> = (0..kinds.len() as u64).map(|k| line(brook, &[0, k])).collect();
        let all = parse_tile(&layer_with("streets", &["kind"], &kinds, &ways), MAP_ZOOM, 8466, 5890).unwrap();
        use RoadKind::*;
        assert_eq!(all.roads.iter().map(|r| r.kind).collect::<Vec<_>>(), [Major, Main, Main, Minor, Service, Pedestrian, Track, Footway, Path, Tram]);
        // The meadow is not scenery; the forest keeps its clearing as a second ring.
        assert_eq!(found.areas.len(), 2);
        assert_eq!((found.areas[0].kind, found.areas[0].rings.len()), (AreaKind::Forest, 2));
        assert_eq!((found.areas[1].kind, found.areas[1].rings.len()), (AreaKind::Water, 1));
        // The ditch and the culverted stream are dropped.
        assert_eq!(found.waterways.len(), 1);
        assert_eq!((found.waterways[0].kind, found.waterways[0].line.len()), (WaterwayKind::Stream, 3));
        // Bridges and tunnels are kept apart from road that lies on the ground, each with what it is.
        assert_eq!(found.crossings.len(), 3);
        let c = &found.crossings;
        assert!(!c[0].tunnel && c[0].kind == RoadKind::Main);
        assert!(c[1].tunnel && !c[1].kind.is_rail());
        assert!(!c[2].tunnel && c[2].kind == RoadKind::Rail);
        assert_eq!(found.roads.len(), 1);
        assert_eq!((found.roads[0].kind, found.roads[0].line.len()), (RoadKind::Main, 3));
        // The bridge runs off the tile's edge at x = 4096 and is cut there, for the next tile to continue.
        let n = 2f64.powi(MAP_ZOOM as i32);
        let edge = 8467.0 / n * 360.0 - 180.0;
        assert_eq!(c[0].line.len(), 2);
        assert!(close(c[0].line[1][1], edge, 1e-6), "{} vs {edge}", c[0].line[1][1]);
        assert!(c[0].line[0][1] < edge);
    }

    #[test]
    fn lists_the_tiles_along_a_route() {
        let route = build_route("flat", &straight(3000.0, |_| Some(100.0))).unwrap();
        let tiles = corridor_tiles(&route, 350.0);
        // 3 km of road plus margins spans only a handful of ~1.7 km tiles, each listed once.
        assert!((2..=9).contains(&tiles.len()), "{} tiles", tiles.len());
        let mut unique = tiles.clone();
        unique.dedup();
        assert_eq!(unique.len(), tiles.len());
        // Every point of the route falls in one of them.
        let n = 2f64.powi(MAP_ZOOM as i32);
        for i in (0..route.n).step_by(50) {
            let rad = route.lat[i].to_radians();
            let key = (((route.lon[i] + 180.0) / 360.0 * n) as u32, ((1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n) as u32);
            assert!(tiles.contains(&key));
        }
    }
}

mod scenery {
    use super::*;
    use wintermiles_core::world::{Area, AreaKind, Crossing, GridMesh, Road, RoadKind, Scenery, Waterway, WaterwayKind, BRIDGE, OPEN, ROAD_SECTION, TUNNEL};

    /// Terrain tiles at a constant height, covering the route.
    fn level_ground(route: &Route, height: f64) -> Dem {
        let (zoom, keys) = Dem::plan(route).unwrap();
        let mut dem = Dem::new(zoom);
        let v = (height + 32768.0) as u32;
        let px: Vec<u8> = (0..TILE * TILE).flat_map(|_| [(v / 256) as u8, (v % 256) as u8, 0]).collect();
        for key in &keys {
            assert!(dem.insert_rgb(*key, &px, 3));
        }
        dem
    }

    /// Local z of the point `along` metres up the road, which heads north (−z) and has its origin half-way.
    fn z(along: f64) -> f64 {
        1500.0 - along
    }

    /// Geographic points for `(x metres east of the road, metres along it)`.
    fn geo(route: &Route, points: &[(f64, f64)]) -> Vec<[f64; 2]> {
        points
            .iter()
            .map(|&(x, along)| {
                let (lat, lon) = route.to_lat_lon(x, z(along));
                [lat, lon]
            })
            .collect()
    }

    fn road(route: &Route, tunnel: bool, rail: bool, points: &[(f64, f64)]) -> Crossing {
        Crossing { tunnel, kind: if rail { RoadKind::Rail } else { RoadKind::Minor }, line: geo(route, points) }
    }

    /// 3 km of level road with: a bridge at 500–600 m, a long tunnel at 1000–1400 m, a short one at
    /// 1800–1830 m, a road crossing overhead at 2200 m and a railway bridge alongside at 2500–2600 m;
    /// a wood to the east and a lake to the west near the start, and a stream beyond the lake.
    fn world() -> (Arc<Route>, World) {
        let route = Arc::new(build_route("scenic", &straight(3000.0, |_| Some(100.0))).unwrap());
        let scenery = Scenery {
            tiles: Vec::new(),
            areas: vec![
                Area { kind: AreaKind::Forest, rings: vec![geo(&route, &[(40.0, 100.0), (200.0, 100.0), (200.0, 300.0), (40.0, 300.0)])] },
                Area {
                    kind: AreaKind::Water,
                    rings: vec![geo(&route, &[(-200.0, 100.0), (-40.0, 100.0), (-40.0, 300.0), (-200.0, 300.0)]), geo(&route, &[(-130.0, 190.0), (-110.0, 190.0), (-110.0, 210.0), (-130.0, 210.0)])],
                },
            ],
            waterways: vec![Waterway { kind: WaterwayKind::Stream, line: geo(&route, &[(-300.0, 320.0), (-60.0, 380.0), (60.0, 380.0)]) }],
            crossings: vec![
                // Mapped three metres to one side of the track, as a GPS trace would have it.
                road(&route, false, false, &[(3.0, 500.0), (3.0, 600.0)]),
                road(&route, true, false, &[(-3.0, 1000.0), (-3.0, 1400.0)]),
                road(&route, true, false, &[(0.0, 1800.0), (0.0, 1830.0)]),
                road(&route, false, false, &[(-20.0, 2195.0), (20.0, 2205.0)]),
                road(&route, false, true, &[(6.0, 2500.0), (6.0, 2600.0)]),
                // A side road's bridge, well clear of the route.
                road(&route, false, false, &[(300.0, 2700.0), (360.0, 2700.0)]),
            ],
            roads: vec![
                // The road the route itself follows, as mapped; a main road across it at 700 m;
                // a lane through the wood; a railway beside the lake; and the road to that far bridge.
                Road { kind: RoadKind::Minor, line: geo(&route, &[(2.0, 0.0), (2.0, 480.0)]) },
                Road { kind: RoadKind::Main, line: geo(&route, &[(-400.0, 660.0), (0.0, 700.0), (400.0, 740.0)]) },
                Road { kind: RoadKind::Track, line: geo(&route, &[(40.0, 200.0), (200.0, 200.0)]) },
                Road { kind: RoadKind::Rail, line: geo(&route, &[(-260.0, 100.0), (-260.0, 600.0)]) },
                Road { kind: RoadKind::Minor, line: geo(&route, &[(100.0, 2700.0), (300.0, 2700.0)]) },
            ],
        };
        let dem = level_ground(&route, 100.0);
        let world = World::with_map(route.clone(), Some(dem), 0.0, Some(&scenery), &[]);
        (route, world)
    }

    #[test]
    fn finds_the_bridges_and_tunnels_the_route_uses() {
        let (route, world) = world();
        let kinds = world.field.kinds();
        let at = |m: f64| kinds[(m / route.step) as usize];
        assert_eq!([at(400.0), at(550.0), at(700.0)], [OPEN, BRIDGE, OPEN]);
        assert_eq!([at(900.0), at(1200.0), at(1500.0)], [OPEN, TUNNEL, OPEN]);
        // The short tunnel is open road under a slab; the railway alongside is not the cyclist's bridge.
        assert_eq!([at(1815.0), at(2550.0)], [OPEN, OPEN]);
        let c = world.scene.counts;
        assert_eq!((c.forests, c.bridges, c.tunnels, c.overbridges), (1, 1, 1, 2));
        // Both tunnels count as under cover, for the camera.
        assert_eq!(world.scene.covered.len(), 2);
        assert!(close(world.scene.covered[0][0], 1000.0, 10.0) && close(world.scene.covered[0][1], 1400.0, 10.0));
        assert!(close(world.scene.covered[1][0], 1800.0, 10.0));
        // What crosses overhead leaves headroom, and its supports stand back from the road.
        for over in &world.scene.overbridges {
            assert!(over.deck > 105.0);
            let ends = [over.line[0], *over.line.last().unwrap()];
            assert!(ends.iter().all(|e| e.0.abs() > 8.0), "{ends:?}");
        }
    }

    #[test]
    fn shapes_the_ground_for_bridges_and_tunnels() {
        let (_, world) = world();
        let f = &world.field;
        // Open road: ground at road level beside it.
        assert!(close(f.height(5.0, z(300.0)).0, 99.65, 0.1));
        // Under the middle of the bridge the ground has dropped away, and the deck is still "the road" to anything nearby.
        let under = f.probe(0.0, z(550.0), false);
        assert!(under.ground < 96.0 && under.road < 3.0, "{} {}", under.ground, under.road);
        assert!(under.open > 40.0);
        // In the tunnel the mesh is a trench under the road, and the land you would stand on is above the bore.
        let inside = f.probe(0.0, z(1200.0), false);
        assert!(inside.trench && inside.ground < 99.0 && inside.surface > 106.5, "{} {}", inside.ground, inside.surface);
        // The trench is no wider than it needs to be: thirty metres out the mesh is back at the surface.
        let beside = f.probe(30.0, z(1200.0), false);
        assert!(!beside.trench && close(beside.ground, beside.surface, 1e-9) && beside.surface > 106.0);
        // Nothing overhead is "the road" for the purposes of keeping trees and houses off it.
        assert!(f.surface(0.0, z(1200.0)).1 > 80.0);
        // Approaching the mouth, the open road's cutting is trench-sized, so the land beside it is already up.
        assert!(f.height(30.0, z(990.0)).0 > 105.0);
        assert!(close(f.height(5.0, z(990.0)).0, 99.65, 0.3));
    }

    #[test]
    fn fills_lakes_plants_forests_and_lays_streams() {
        let (_, world) = world();
        // 100–300 m along is z 1200–1400, all in chunk row 5 (z 1200–1440): chunk (0, 5) east of the road, (−1, 5) west.
        let woods = world.chunk(0, 5);
        let lake = world.chunk(-1, 5);
        let trees = |m: &wintermiles_core::world::GridMesh| m.trees.chunks(6).map(|t| (t[0] as f64, t[2] as f64)).collect::<Vec<_>>();
        let in_forest = trees(&woods).iter().filter(|t| t.0 >= 40.0 && t.0 <= 200.0 && t.1 >= 1200.0 && t.1 <= 1400.0).count();
        // 160 m × 200 m of forest at one tree per ten-metre cell, less the thinning.
        assert!(in_forest > 120, "{in_forest} trees");
        assert!(woods.water.is_empty());

        assert!(!lake.water.is_empty());
        assert_eq!(lake.water.len(), lake.water_colors.len());
        // Still water: one level throughout, just below the banks.
        for v in lake.water.chunks(3) {
            assert!(v[1] > 99.0 && v[1] < 99.8, "water at {}", v[1]);
            assert!(v[0] >= -211.0 && v[0] <= -29.0 && v[2] >= 1189.0 && v[2] <= 1411.0, "water at {}, {}", v[0], v[2]);
        }
        // The bed is below the surface, the island in the middle is dry, and no tree stands in the lake.
        let ground = |m: &wintermiles_core::world::GridMesh, x: f32, z: f32| m.positions.chunks(3).find(|p| p[0] == x && p[2] == z).unwrap()[1];
        assert!(ground(&lake, -80.0, 1300.0) < 98.6);
        assert!(ground(&lake, -120.0, 1300.0) > 99.9);
        assert!(trees(&lake).iter().all(|t| !(t.0 > -200.0 && t.0 < -40.0 && t.1 > 1200.0 && t.1 < 1400.0) || ((t.0 + 120.0).abs() < 15.0 && (t.1 - 1300.0).abs() < 15.0)), "{:?}", trees(&lake));

        // The stream is a ribbon on the ground, stopping short of the road it would otherwise cross.
        let (far_west, west, east) = (world.chunk(-2, 4), world.chunk(-1, 4), world.chunk(0, 4));
        let ribbon: Vec<&[f32]> = far_west.water.chunks(3).chain(west.water.chunks(3)).chain(east.water.chunks(3)).collect();
        assert!(ribbon.len() > 100, "{} vertices", ribbon.len());
        assert!(ribbon.iter().all(|v| v[1] > 99.5 && v[1] < 101.5 && v[0].abs() > 5.0));
        assert!(ribbon.iter().any(|v| v[0] < -200.0) && ribbon.iter().any(|v| v[0] > 30.0));
        // Every water triangle, lake or stream, faces up: the renderer only draws that side.
        for mesh in [&lake, &far_west, &west, &east] {
            for t in mesh.water.chunks(9) {
                let (u, v) = ([t[3] - t[0], t[5] - t[2]], [t[6] - t[0], t[8] - t[2]]);
                assert!(u[1] * v[0] - u[0] * v[1] > 0.0, "triangle faces down: {t:?}");
            }
        }
    }

    #[test]
    fn lays_the_other_roads_on_the_ground() {
        let (_, world) = world();
        let c = world.scene.counts;
        // The mapped copy of the route's own road is not drawn a second time.
        assert_eq!((c.roads, c.road_bridges), (4, 1));
        let vertices = |m: &GridMesh| m.roads.positions.chunks(3).map(|v| [v[0] as f64, v[1] as f64, v[2] as f64]).collect::<Vec<_>>();
        // The main road crosses at 700 m (z 800): chunk row 3 holds z 720–960.
        let (west, east) = (world.chunk(-1, 3), world.chunk(0, 3));
        for (mesh, cx) in [(&west, -1.0), (&east, 0.0)] {
            let r = &mesh.roads;
            assert!(r.vertices() > 100, "{} vertices", r.vertices());
            assert_eq!((r.normals.len(), r.colors.len(), r.uvs.len()), (r.positions.len(), r.positions.len(), r.vertices() * 2));
            for (k, t) in vertices(mesh).chunks(3).enumerate() {
                // Cut to the chunk, lying on the ground (road level by the route, natural beyond), facing up.
                assert!(t.iter().all(|v| v[0] >= cx * CHUNK - 1e-3 && v[0] <= (cx + 1.0) * CHUNK + 1e-3 && v[2] >= 3.0 * CHUNK - 1e-3 && v[2] <= 4.0 * CHUNK + 1e-3), "{t:?}");
                for v in t {
                    let ground = on_ground(mesh, v[0], v[2]);
                    // …except at the junction, where it comes up off the verge to the route's tarmac
                    // and stops a little way onto it rather than running underneath.
                    let top = if v[0].abs() < 7.0 { 100.12 } else { ground + 0.08 };
                    assert!(v[1] > ground + 0.02 && v[1] < top, "road at {} over ground at {ground}", v[1]);
                    assert!(v[0].abs() > 1.7, "road under the route at x = {}", v[0]);
                }
                let (u, w) = ([t[1][0] - t[0][0], t[1][2] - t[0][2]], [t[2][0] - t[0][0], t[2][2] - t[0][2]]);
                assert!(u[1] * w[0] - u[0] * w[1] > 0.0, "triangle faces down: {t:?}");
                assert!(r.normals[k * 9 + 1] > 0.5);
                // A main road has lines on it: its texture coordinates span the marked part of the texture.
                assert!(r.uvs[k * 6..k * 6 + 6].chunks(2).all(|uv| (0.0..=0.376).contains(&uv[0])));
            }
            // Seven and a half metres wide, all the way across the chunk.
            let across = vertices(mesh).iter().filter(|v| (v[0] - (cx * CHUNK + 100.0)).abs() < 20.0).map(|v| v[2]).fold((f64::MAX, f64::MIN), |(lo, hi), z| (lo.min(z), hi.max(z)));
            assert!(across.1 - across.0 > 7.5 && across.1 - across.0 < 13.0, "{across:?}");
            assert!(vertices(mesh).iter().any(|v| v[0] < cx * CHUNK + 10.0) && vertices(mesh).iter().any(|v| v[0] > if cx < 0.0 { -10.0 } else { 230.0 }));
            // It meets the route's tarmac (at 100.07) at its edge.
            assert!(vertices(mesh).iter().any(|v| v[0].abs() < 3.5 && v[1] > 100.07));
        }
        // Nothing of the route's own road in the chunks it runs through; only the track in the wood.
        let woods = world.chunk(0, 5);
        assert!(woods.roads.vertices() > 0);
        assert!(vertices(&woods).iter().all(|v| v[0] > 38.0 && (v[2] - z(200.0)).abs() < 2.0), "something besides the track");
        // …and no tree stands on the track.
        assert!(woods.trees.chunks(6).all(|t| !(t[0] > 40.0 && t[0] < 200.0 && (t[2] as f64 - z(200.0)).abs() < 2.5)));
        // The railway is a bed with two rails on top, drawn after it.
        let rail = world.chunk(-2, 5);
        let dark = rail.roads.colors.chunks(3).position(|c| c[0] < 0.05).expect("rails");
        assert!(dark > 0 && rail.roads.colors.chunks(3).skip(dark).all(|c| c[0] < 0.05));
        assert!(vertices(&rail).iter().all(|v| (v[0] + 260.0).abs() < 1.8));
    }

    /// Height of a chunk's ground mesh under a point.
    fn on_ground(mesh: &GridMesh, x: f64, z: f64) -> f64 {
        let (cell, row) = (CHUNK / 24.0, 25);
        let (x0, z0) = (mesh.positions[0] as f64, mesh.positions[2] as f64);
        let (fx, fz) = (((x - x0) / cell).clamp(0.0, 24.0 - 1e-9), ((z - z0) / cell).clamp(0.0, 24.0 - 1e-9));
        let (i, j) = (fx as usize, fz as usize);
        let (u, v) = (fx - i as f64, fz - j as f64);
        let h = |di: usize, dj: usize| mesh.positions[((j + dj) * row + i + di) * 3 + 1] as f64;
        if u + v <= 1.0 {
            h(0, 0) + (h(1, 0) - h(0, 0)) * u + (h(0, 1) - h(0, 0)) * v
        } else {
            h(1, 1) + (h(0, 1) - h(1, 1)) * (1.0 - u) + (h(1, 0) - h(1, 1)) * (1.0 - v)
        }
    }

    #[test]
    fn builds_decks_bores_and_overbridges() {
        let (route, world) = world();
        // On the bridge the road's edge is a slab, not a retaining wall.
        let ribbon = world.road();
        let edge = |m: f64| ribbon.positions[(m / route.step) as usize * ROAD_SECTION * 3 + 1];
        assert!(edge(300.0) < 90.0 && edge(550.0) > 98.0);

        let s = world.structures();
        assert_eq!(s.lit.positions.len(), s.lit.normals.len());
        assert_eq!(s.lit.positions.len(), s.lit.colors.len());
        assert!(s.lit.normals.iter().all(|v| v.is_finite()));
        // The unlit mesh is the long tunnel's bore and lamps, and nothing else.
        assert!(s.unlit.vertices() > 400);
        for v in s.unlit.positions.chunks(3) {
            assert!(v[2] <= z(985.0) as f32 && v[2] >= z(1415.0) as f32 && v[0].abs() < 5.0 && v[1] < 106.0, "{v:?}");
        }
        let lit: Vec<&[f32]> = s.lit.positions.chunks(3).collect();
        let between = |v: &[f32], from: f64, to: f64| v[2] < z(from) as f32 && v[2] > z(to) as f32;
        // Land over the tunnel, above the bore.
        assert!(lit.iter().any(|v| between(v, 1100.0, 1300.0) && v[0].abs() < 4.0 && v[1] > 106.0));
        // Parapets on the bridge; a slab over the short tunnel; the road crossing at 2200 m.
        assert!(lit.iter().any(|v| between(v, 510.0, 590.0) && v[1] > 100.5 && v[1] < 101.2));
        assert!(lit.iter().any(|v| between(v, 1790.0, 1840.0) && v[0].abs() < 30.0 && v[1] > 105.0));
        assert!(lit.iter().any(|v| between(v, 2180.0, 2220.0) && v[0].abs() < 40.0 && v[1] > 105.0));
        // The side road's bridge at 2700 m, three hundred metres off: a deck at ground level with parapets.
        let deck: Vec<&&[f32]> = lit.iter().filter(|v| v[0] > 295.0 && v[0] < 365.0 && between(v, 2690.0, 2710.0)).collect();
        assert!(deck.len() >= 24 * 10, "{} vertices", deck.len());
        let ground = world.field.surface(330.0, z(2700.0)).0 as f32;
        assert!(deck.iter().all(|v| v[1] > ground - 3.0 && v[1] < ground + 3.0) && deck.iter().any(|v| v[1] > ground + 0.9));
        // Nothing hangs in the bore of the open road: no structure intrudes on the space riders use, outside the bridge parapets.
        assert!(lit.iter().all(|v| !(v[0].abs() < 3.0 && v[1] > 100.3 && v[1] < 104.5)), "something is in the road");
    }

    #[test]
    fn a_road_with_no_elevation_runs_level_through_a_hill_and_over_a_valley() {
        use wintermiles_core::world::{align_dem_with, route_structures};
        // Terrain that is flat at 1000 m, with a 100 m ridge across the road at 1000–1400 m along
        // and a 60 m deep valley at 2000–2200 m. (North is up; a tile row is about 27 m here.)
        let mut route = build_route("planner", &straight(3000.0, |_| None)).unwrap();
        let (zoom, keys) = Dem::plan(&route).unwrap();
        let m_per_row = 40_075_016.7 * 45f64.to_radians().cos() / 2f64.powi(zoom as i32) / TILE as f64;
        let n = 2f64.powi(zoom as i32);
        let row_of = |lat: f64| (1.0 - (lat.to_radians().tan() + 1.0 / lat.to_radians().cos()).ln() / std::f64::consts::PI) / 2.0 * n * TILE as f64;
        let start_row = row_of(route.lat[0]);
        let mut dem = Dem::new(zoom);
        for key in &keys {
            let mut px = vec![0u8; TILE * TILE * 3];
            for y in 0..TILE {
                let along = (start_row - (key.1 as f64 * TILE as f64 + y as f64)) * m_per_row;
                let h = 1000.0 + if (1000.0..1400.0).contains(&along) { 100.0 } else if (2000.0..2200.0).contains(&along) { -60.0 } else { 0.0 };
                let v = (h + 32768.0) as u32;
                for x in 0..TILE {
                    px[(y * TILE + x) * 3] = (v / 256) as u8;
                    px[(y * TILE + x) * 3 + 1] = (v % 256) as u8;
                }
            }
            assert!(dem.insert_rgb(*key, &px, 3));
        }
        let scenery = Scenery {
            crossings: vec![road(&route, true, false, &[(0.0, 950.0), (0.0, 1450.0)]), road(&route, false, false, &[(0.0, 1950.0), (0.0, 2250.0)])],
            ..Default::default()
        };
        // Without knowing about them, the road goes over the ridge and down into the valley.
        let mut naive = route.clone();
        align_dem(&mut naive, &dem);
        let at = |r: &Route, m: f64| r.ele[(m / r.step) as usize] as f64;
        assert!(at(&naive, 1200.0) > 1080.0 && at(&naive, 2100.0) < 960.0, "{} {}", at(&naive, 1200.0), at(&naive, 2100.0));
        // Knowing, it stays level through the tunnel and across the bridge.
        let structures = route_structures(&route, &scenery);
        assert_eq!(structures[(1200.0 / route.step) as usize], TUNNEL);
        align_dem_with(&mut route, &dem, &structures);
        assert!(close(at(&route, 1200.0), 1000.0, 3.0) && close(at(&route, 2100.0), 1000.0, 3.0), "{} {}", at(&route, 1200.0), at(&route, 2100.0));
        assert!(route.ascent < 10.0 && route.climbs.is_empty(), "ascent {}", route.ascent);
    }

    #[test]
    fn a_world_without_scenery_is_unchanged() {
        let route = Arc::new(build_route("plain", &straight(1000.0, |_| Some(100.0))).unwrap());
        let (plain, mapped) = (World::new(route.clone(), None, 0.0), World::with_map(route.clone(), None, 0.0, Some(&Scenery::default()), &[]));
        let (a, b) = (plain.chunk(0, -1), mapped.chunk(0, -1));
        assert_eq!(a.positions, b.positions);
        assert_eq!(a.trees, b.trees);
        assert!(b.water.is_empty() && b.roads.vertices() == 0 && mapped.structures().lit.vertices() == 0);
    }
}
