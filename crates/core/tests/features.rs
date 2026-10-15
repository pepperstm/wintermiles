//! Workouts, fitness, segments, ghosts, streaks and trainer feel.

use std::sync::Arc;

use wintermiles_core::fitness::{estimate_ftp, power_curve, power_zone, training_load, CURVE_DURATIONS};
use wintermiles_core::gamify::streaks;
use wintermiles_core::gpx::{build_route, parse_gpx_track_times, parse_iso_time, RawPoint, M_PER_DEG};
use wintermiles_core::ride::{GhostTrace, RideEngine, RideInput, RideOptions};
use wintermiles_core::segments::{decode_polyline, follow_track, ghost_trace, place_segment, Segment, SegmentSource};
use wintermiles_core::types::{Settings, TrainerSettings};
use wintermiles_core::workout::{builtin_workouts, parse_erg, parse_zwo, StepKind};

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// A road heading north for `km`, then east, on flat ground.
fn l_route(km: f64) -> Arc<wintermiles_core::gpx::Route> {
    let mut pts = Vec::new();
    let n = (km * 100.0) as usize;
    for i in 0..=n {
        pts.push(RawPoint { lat: 45.0 + i as f64 * 10.0 / M_PER_DEG, lon: 6.0, ele: Some(100.0) });
    }
    let top = 45.0 + n as f64 * 10.0 / M_PER_DEG;
    let cos = top.to_radians().cos();
    for i in 1..=n {
        pts.push(RawPoint { lat: top, lon: 6.0 + i as f64 * 10.0 / (M_PER_DEG * cos), ele: Some(100.0) });
    }
    Arc::new(build_route("L", &pts).unwrap())
}

const ZWO: &str = r#"<workout_file>
  <name>Test session</name>
  <description>For the tests</description>
  <workout>
    <Warmup Duration="300" PowerLow="0.40" PowerHigh="0.70"/>
    <SteadyState Duration="120" Power="0.9" Cadence="95">
      <textevent timeoffset="10" message="Settle in"/>
    </SteadyState>
    <IntervalsT Repeat="3" OnDuration="30" OffDuration="60" OnPower="1.2" OffPower="0.5"/>
    <FreeRide Duration="60"/>
    <Cooldown Duration="200" PowerLow="0.3" PowerHigh="0.6"/>
  </workout>
</workout_file>"#;

#[test]
fn reads_zwift_workouts() {
    let w = parse_zwo(ZWO, "t").unwrap();
    assert_eq!(w.name, "Test session");
    assert_eq!(w.steps.len(), 1 + 1 + 6 + 1 + 1);
    assert!(close(w.duration(), 300.0 + 120.0 + 3.0 * 90.0 + 60.0 + 200.0, 1e-9));
    // Warm-up ramps up, cool-down ramps down, whichever way round the file wrote them.
    assert!(close(w.at(0.0).unwrap().target, 0.4, 1e-9));
    assert!(close(w.at(150.0).unwrap().target, 0.55, 1e-9));
    let cool = w.steps.last().unwrap();
    assert!(cool.from > cool.to);
    assert_eq!(w.at(300.0 + 60.0).unwrap().cadence, Some(95.0));
    assert!(close(w.at(420.0 + 15.0).unwrap().target, 1.2, 1e-9));
    assert!(close(w.at(420.0 + 45.0).unwrap().target, 0.5, 1e-9));
    assert_eq!(w.at(420.0 + 270.0 + 5.0).unwrap().kind, StepKind::Free);
    assert!(w.at(w.duration() + 1.0).is_none());
    assert_eq!(w.cue_at(315.0), Some("Settle in"));
    assert_eq!(w.cue_at(330.0), None);
}

#[test]
fn reads_erg_and_mrc_workouts() {
    let erg = "[COURSE HEADER]\nVERSION = 2\nUNITS = ENGLISH\nFILE NAME = Over unders\nMINUTES WATTS\n[END COURSE HEADER]\n[COURSE DATA]\n0.00\t100\n5.00\t200\n5.00\t250\n10.00\t250\n[END COURSE DATA]\n";
    let w = parse_erg(erg, "e", "file", 250.0).unwrap();
    assert_eq!(w.name, "Over unders");
    assert_eq!(w.steps.len(), 2);
    assert!(close(w.at(150.0).unwrap().target, 0.6, 1e-9));
    assert!(close(w.at(400.0).unwrap().target, 1.0, 1e-9));
    let mrc = erg.replace("MINUTES WATTS", "MINUTES PERCENT");
    let m = parse_erg(&mrc, "m", "file", 0.0).unwrap();
    assert!(close(m.at(400.0).unwrap().target, 2.5, 1e-9));
    assert!(parse_erg(erg, "e", "file", 0.0).is_err());
}

#[test]
fn built_in_workouts_are_sensible() {
    for w in builtin_workouts() {
        assert!(w.duration() >= 20.0 * 60.0, "{} is too short", w.name);
        let tss = w.stress();
        if w.id == "builtin-ramp-test" {
            // Nobody rides it to the end, so its nominal stress is high.
            continue;
        }
        assert!(tss > 20.0 && tss < 200.0, "{} has stress {tss}", w.name);
    }
    // An hour at exactly FTP is 100 TSS.
    let hour = parse_zwo(r#"<workout_file><workout><SteadyState Duration="3600" Power="1.0"/></workout></workout_file>"#, "h").unwrap();
    assert!(close(hour.stress(), 100.0, 0.01));
}

#[test]
fn erg_workout_drives_the_trainer_target() {
    let route = l_route(2.0);
    let mut settings = Settings::default();
    settings.rider.ftp = 250.0;
    let workout = parse_zwo(ZWO, "t").unwrap();
    let mut engine = RideEngine::with_options(route, &settings, None, RideOptions { workout: Some(workout), ..Default::default() });
    for i in 0..(301 * 10) {
        engine.tick(0.1, RideInput { power: 200.0, ..Default::default() }, 1_700_000_000_000 + i * 100);
    }
    // Into the steady block at 90% of FTP.
    assert!(close(engine.workout_target().unwrap(), 225.0, 1.0));
    engine.intensity = 1.1;
    assert!(close(engine.workout_target().unwrap(), 247.5, 1.0));
}

#[test]
fn power_curve_and_ftp() {
    // 30 minutes: 10 at 150 W, 20 at 280 W.
    let mut watts = vec![150.0; 600];
    watts.extend(vec![280.0; 1200]);
    let curve = power_curve(&watts);
    assert_eq!(curve.len(), CURVE_DURATIONS.len());
    assert!(close(curve[0], 280.0, 1e-9));
    assert!(close(curve[7], 280.0, 1e-9));
    assert_eq!(curve[9], 0.0);
    let est = estimate_ftp(&curve, false).unwrap();
    assert!(close(est.watts, 266.0, 0.5));
    // A ramp test's best minute.
    let mut ramp = power_curve(&(0..1200).map(|s| 100.0 + (s / 60) as f64 * 20.0).collect::<Vec<_>>());
    ramp[7] = 0.0;
    assert!(close(estimate_ftp(&ramp, true).unwrap().watts, 0.75 * 480.0, 0.5));
    assert_eq!(power_zone(100.0, 200.0), 1);
    assert_eq!(power_zone(200.0, 200.0), 4);
    assert_eq!(power_zone(400.0, 200.0), 7);
}

#[test]
fn training_load_rises_and_falls() {
    let rides: Vec<(i64, f64)> = (0..42).map(|d| (1000 + d, 100.0)).collect();
    let load = training_load(&rides, 1000 + 60);
    assert_eq!(load.len(), 61);
    let peak = &load[41];
    assert!(peak.fitness > 60.0 && peak.fitness < 70.0, "{}", peak.fitness);
    assert!(peak.fatigue > 99.0);
    assert!(peak.form < 0.0);
    // After rest, fatigue drops faster than fitness: form turns positive.
    assert!(load.last().unwrap().form > 0.0);
}

#[test]
fn streaks_count_weeks_and_days() {
    // Day 0 (1 Jan 1970) was a Thursday; 4 = Monday 5 Jan.
    let today = 4 + 7 * 10 + 2; // a Wednesday
    let days = [today, today - 1, today - 7, today - 14, today - 21, today - 35];
    let s = streaks(&days, today);
    assert_eq!(s.days, 2);
    assert_eq!(s.weeks, 4);
    assert_eq!(s.best_weeks, 4);
    // No ride yet this week still keeps last week's streak.
    assert_eq!(streaks(&days[2..], today).weeks, 3);
    assert_eq!(streaks(&[], today).weeks, 0);
}

#[test]
fn trainer_feel_scales_softens_and_caps() {
    let t = TrainerSettings { difficulty: 0.5, downhill: 0.5, max_grade: 0.08, ..Default::default() };
    assert!(close(t.trainer_grade(0.10), 0.05, 1e-12));
    assert!(close(t.trainer_grade(0.30), 0.08, 1e-12));
    assert!(close(t.trainer_grade(-0.10), -0.025, 1e-12));
    let uncapped = TrainerSettings { difficulty: 1.0, downhill: 1.0, max_grade: 0.0, ..Default::default() };
    assert!(close(uncapped.trainer_grade(0.3), 0.3, 1e-12));
}

#[test]
fn decodes_polylines() {
    // Google's documented example.
    let pts = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@");
    assert_eq!(pts.len(), 3);
    assert!(close(pts[0].0, 38.5, 1e-9) && close(pts[0].1, -120.2, 1e-9));
    assert!(close(pts[2].0, 43.252, 1e-9) && close(pts[2].1, -126.453, 1e-9));
}

#[test]
fn places_segments_and_follows_tracks() {
    let route = l_route(2.0);
    // A segment along the eastward leg, from 2.5 km to 3.5 km.
    let at = |d: f64| {
        let p = route.sample_at(d);
        (p.lat + 0.00003, p.lon)
    };
    let shape: Vec<(f64, f64)> = (0..=20).map(|i| at(2500.0 + i as f64 * 50.0)).collect();
    let (s, e) = place_segment(&route, &shape, 1000.0).unwrap();
    assert!(close(s, 2500.0, 15.0) && close(e, 3500.0, 15.0), "{s} {e}");
    // The wrong length, or a shape that leaves the route, doesn't match.
    assert!(place_segment(&route, &shape, 2000.0).is_none());
    let mut off = shape.clone();
    off[10].0 += 0.01;
    assert!(place_segment(&route, &off, 1000.0).is_none());

    let followed = follow_track(&route, &shape, 20.0);
    assert!(followed.iter().all(|d| d.is_some()));
    assert!(followed.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn ghosts_from_recorded_rides() {
    let route = l_route(2.0);
    // Waits 20 s at the start, then rides 8 m/s, sampled every 2 s.
    let mut track = Vec::new();
    for t in 0..10 {
        let p = route.sample_at(0.0);
        track.push((1000.0 + t as f64 * 2.0, p.lat, p.lon));
    }
    for k in 1..200 {
        let p = route.sample_at((k as f64 * 16.0).min(route.total));
        track.push((1018.0 + k as f64 * 2.0, p.lat, p.lon));
    }
    let trace = ghost_trace(&route, &track).unwrap();
    assert!(close(trace[0], 0.0, 1.0));
    assert!(close(trace[100], 800.0, 20.0), "{}", trace[100]);
    assert!(trace.windows(2).all(|w| w[1] >= w[0]));

    let mut engine = RideEngine::with_options(route.clone(), &Settings::default(), None, RideOptions { ghosts: vec![GhostTrace { id: "g".into(), name: "Friend".into(), color: 1, trace: Arc::new(trace) }], ..Default::default() });
    for i in 0..(60 * 10) {
        engine.tick(0.1, RideInput { power: 200.0, ..Default::default() }, 1_700_000_000_000 + i * 100);
    }
    let g = engine.rivals.iter().find(|r| r.id == "g").unwrap();
    assert!(close(g.distance, 480.0, 25.0), "{}", g.distance);
}

#[test]
fn segment_bests_and_live_gap() {
    let route = l_route(1.0);
    let seg = Segment { id: "s".into(), name: "Sprint".into(), source: SegmentSource::Strava, start: 100.0, end: 600.0, avg_grade: 0.0, record: None };
    let ride = |power: f64, bests| {
        let mut e = RideEngine::with_options(route.clone(), &Settings::default(), None, RideOptions { segments: vec![seg.clone()], segment_bests: bests, ..Default::default() });
        let mut live_gap = None;
        for i in 0..(200 * 10) {
            e.tick(0.1, RideInput { power, ..Default::default() }, 1_700_000_000_000 + i * 100);
            if let Some(l) = e.segment_live().filter(|l| l.to_go < 100.0) {
                live_gap = l.gap.or(live_gap);
            }
        }
        (e, live_gap)
    };
    let (first, gap) = ride(150.0, Default::default());
    assert!(gap.is_none());
    let best = first.new_segment_bests.get("s").unwrap().clone();
    assert!(best.time_s > 30.0 && best.splits.len() >= 50);
    let (second, gap) = ride(300.0, [("s".to_string(), best.clone())].into());
    // Faster: ahead of the best effort, and a new best.
    assert!(gap.unwrap() < -3.0, "{gap:?}");
    let result = second.last_segment_result.as_ref().unwrap();
    assert_eq!(result.previous_best, Some(best.time_s));
    assert!(second.new_segment_bests.get("s").unwrap().time_s < best.time_s);
}

#[test]
fn reads_gpx_times() {
    assert_eq!(parse_iso_time("1970-01-01T00:00:00Z"), Some(0.0));
    assert_eq!(parse_iso_time("2024-03-01T12:30:15Z"), Some(1_709_296_215.0));
    assert_eq!(parse_iso_time("2024-03-01T13:30:15.500+01:00"), Some(1_709_296_215.5));
    let gpx = format!(
        "<gpx><trk><trkseg>{}</trkseg></trk></gpx>",
        (0..12).map(|i| format!("<trkpt lat=\"45.{i:02}\" lon=\"6\"><time>2024-03-01T12:00:{i:02}Z</time></trkpt>")).collect::<String>()
    );
    let pts = parse_gpx_track_times(&gpx).unwrap();
    assert_eq!(pts.len(), 12);
    assert!(close(pts[11].0 - pts[0].0, 11.0, 1e-9));
    assert!(parse_gpx_track_times("<gpx><trk><trkseg><trkpt lat=\"1\" lon=\"2\"/></trkseg></trk></gpx>").is_err());
}

#[test]
fn landmarks_along_the_route() {
    use wintermiles_core::world::{landmarks, Label, Scenery};
    let route = l_route(2.0);
    let at = |d: f64, east_m: f64| {
        let p = route.sample_at(d);
        [p.lat, p.lon + east_m / (M_PER_DEG * p.lat.to_radians().cos())]
    };
    let scenery = Scenery {
        labels: vec![
            Label { kind: "village".into(), name: "Le Bourg".into(), at: at(1000.0, 200.0) },
            Label { kind: "village".into(), name: "Far Away".into(), at: at(1000.0, 5000.0) },
            Label { kind: "cafe".into(), name: "Café du Col".into(), at: at(500.0, 20.0) },
            Label { kind: "cafe".into(), name: "Off the road".into(), at: at(500.0, 300.0) },
            Label { kind: "water".into(), name: String::new(), at: at(700.0, -10.0) },
        ],
        ..Default::default()
    };
    let marks = landmarks(&route, Some(&scenery));
    let names: Vec<&str> = marks.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Café du Col", "Drinking water", "Le Bourg"], "{marks:?}");
    let village = marks.iter().find(|m| m.name == "Le Bourg").unwrap();
    // Before the closest approach, so the sign comes on entering.
    assert!(village.distance < 1000.0 && village.distance > 700.0);
    // East of a northbound road and west of it are opposite sides.
    let cafe = &marks[0];
    let water = &marks[1];
    assert_eq!(cafe.side, -water.side);
}

#[test]
fn picks_photos_facing_along_the_route() {
    use wintermiles_core::segments::pick_photos;
    let route = l_route(1.0);
    let p = |d: f64| route.sample_at(d);
    let c = |id: &str, d: f64, bearing: f64, t: u64| (id.to_string(), p(d).lat, p(d).lon, bearing, format!("https://x/{id}"), t);
    // The first leg heads north (bearing 0), the second east (90).
    let photos = pick_photos(
        &route,
        &[c("a", 100.0, 5.0, 1), c("b", 110.0, 355.0, 2), c("back", 300.0, 180.0, 3), c("c", 1500.0, 92.0, 4), c("north-on-east-leg", 1600.0, 0.0, 5)],
        30.0,
    );
    let ids: Vec<&str> = photos.iter().map(|p| p.id.as_str()).collect();
    // Same 30 m slot: the newer one wins. Looking backwards or sideways: dropped.
    assert_eq!(ids, ["b", "c"]);
}
