//! GPX → a route resampled at a fixed spacing, with smoothed elevation,
//! gradients, detected climbs and a local metric projection for 3D.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawPoint {
    pub lat: f64,
    pub lon: f64,
    pub ele: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Climb {
    /// Start/end as distance along the route in metres.
    pub start: f64,
    pub end: f64,
    pub gain: f64,
    pub length: f64,
    pub avg_grade: f64,
    /// "HC", "1"–"4", or empty for an uncategorised rise.
    pub category: String,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub name: String,
    pub step: f64,
    pub n: usize,
    pub total: f64,
    pub lat0: f64,
    pub lon0: f64,
    /// Local east (x) / south (z) metres from the route centroid — the 3D ground plane.
    pub x: Vec<f64>,
    pub z: Vec<f64>,
    pub ele: Vec<f32>,
    /// Gradient as a fraction (0.05 = 5%).
    pub grade: Vec<f32>,
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    pub ascent: f64,
    pub descent: f64,
    pub min_ele: f64,
    pub max_ele: f64,
    pub climbs: Vec<Climb>,
    pub has_elevation: bool,
    /// True for a loop: the last sample is back where the first one is.
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RoutePos {
    pub x: f64,
    pub z: f64,
    pub ele: f64,
    pub grade: f64,
    pub lat: f64,
    pub lon: f64,
}

const EARTH_R: f64 = 6_371_008.8;
pub const M_PER_DEG: f64 = EARTH_R * std::f64::consts::PI / 180.0;

pub fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2) + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_R * a.sqrt().min(1.0).asin()
}

/// Extracts track points (falling back to route points) and the route name from GPX text.
pub fn parse_gpx(text: &str) -> Result<(String, Vec<RawPoint>), String> {
    let text = text.trim_start_matches('\u{feff}');
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, opts).map_err(|e| format!("This is not a valid GPX file ({e})."))?;

    let collect = |tag: &str| -> Vec<RawPoint> {
        doc.descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == tag)
            .filter_map(|n| {
                let lat: f64 = n.attribute("lat")?.trim().parse().ok()?;
                let lon: f64 = n.attribute("lon")?.trim().parse().ok()?;
                if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 {
                    return None;
                }
                let ele = n
                    .children()
                    .find(|c| c.is_element() && c.tag_name().name() == "ele")
                    .and_then(|c| c.text())
                    .and_then(|t| t.trim().parse::<f64>().ok())
                    .filter(|e| e.is_finite());
                Some(RawPoint { lat, lon, ele })
            })
            .collect()
    };
    let mut points = collect("trkpt");
    if points.len() < 2 {
        points = collect("rtept");
    }
    if points.len() < 2 {
        return Err("No track or route points found in this GPX file.".into());
    }

    let name_in = |container: &str| -> Option<String> {
        doc.descendants()
            .find(|n| n.is_element() && n.tag_name().name() == container)?
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == "name")?
            .text()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    let name = name_in("trk").or_else(|| name_in("rte")).or_else(|| name_in("metadata")).unwrap_or_default();
    Ok((name, points))
}

fn box_smooth(src: &[f32], radius: usize, wrap: bool) -> Vec<f32> {
    let n = src.len();
    if wrap && n > 2 * radius + 1 {
        // A closed loop: the window runs on past the finish into the start.
        let width = (2 * radius + 1) as f64;
        let at = |i: isize| src[i.rem_euclid(n as isize) as usize] as f64;
        let mut sum: f64 = (-(radius as isize)..=radius as isize).map(at).sum();
        return (0..n as isize)
            .map(|i| {
                let v = (sum / width) as f32;
                sum += at(i + radius as isize + 1) - at(i - radius as isize);
                v
            })
            .collect();
    }
    // Prefix sums make the window average O(1) per sample.
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + src[i] as f64;
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(radius);
            let b = (i + radius).min(n - 1);
            ((prefix[b + 1] - prefix[a]) / (b - a + 1) as f64) as f32
        })
        .collect()
}

fn categorise(length: f64, avg_grade: f64) -> &'static str {
    let score = length * avg_grade * 100.0;
    match score {
        s if s >= 80_000.0 => "HC",
        s if s >= 64_000.0 => "1",
        s if s >= 32_000.0 => "2",
        s if s >= 16_000.0 => "3",
        s if s >= 8_000.0 => "4",
        _ => "",
    }
}

fn detect_climbs(route: &Route) -> Vec<Climb> {
    let (ele, grade, n, step) = (&route.ele, &route.grade, route.n, route.step);
    let mut climbs = Vec::new();
    let mut i = 0;
    while i + 1 < n {
        if grade[i] < 0.02 {
            i += 1;
            continue;
        }
        let start = i;
        let mut peak = i;
        let mut j = i;
        while j + 1 < n {
            j += 1;
            if ele[j] > ele[peak] {
                peak = j;
            } else if ele[peak] - ele[j] > 10.0 || (j - peak) as f64 * step > 300.0 {
                // The climb is over after a real descent or a long plateau past the high point.
                break;
            }
        }
        let gain = (ele[peak] - ele[start]) as f64;
        let length = (peak - start) as f64 * step;
        if gain >= 30.0 && length >= 400.0 && gain / length >= 0.03 {
            let avg_grade = gain / length;
            climbs.push(Climb {
                start: start as f64 * step,
                end: peak as f64 * step,
                gain,
                length,
                avg_grade,
                category: categorise(length, avg_grade).to_string(),
            });
        }
        i = peak.max(i + 1);
    }
    climbs
}

impl Route {
    /// Smooths elevation in place and derives gradient, totals and climbs.
    /// Called at build time and again if elevation is back-filled from terrain data.
    pub fn finalize_elevation(&mut self, smooth_m: f64) {
        let (n, step) = (self.n, self.step);
        // GPS/barometric elevation is noisy at the metre scale; two passes of a box
        // filter approximate a Gaussian and stop the trainer resistance twitching.
        let radius = ((smooth_m / step).round() as usize).max(1);
        // On a loop the start and finish are the same place and must end up at the same height.
        self.closed = (self.x[0] - self.x[n - 1]).hypot(self.z[0] - self.z[n - 1]) < 15.0 && (self.ele[0] - self.ele[n - 1]).abs() < 5.0;
        self.ele = box_smooth(&box_smooth(&self.ele, radius, self.closed), radius, self.closed);
        let k = ((10.0 / step).round() as usize).max(1);
        let (mut ascent, mut descent) = (0.0f64, 0.0f64);
        let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..n {
            let a = i.saturating_sub(k);
            let b = (i + k).min(n - 1);
            let g = if b > a { (self.ele[b] - self.ele[a]) as f64 / ((b - a) as f64 * step) } else { 0.0 };
            self.grade[i] = g.clamp(-0.25, 0.25) as f32;
            if i > 0 {
                let d = (self.ele[i] - self.ele[i - 1]) as f64;
                if d > 0.0 {
                    ascent += d;
                } else {
                    descent -= d;
                }
            }
            min = min.min(self.ele[i] as f64);
            max = max.max(self.ele[i] as f64);
        }
        self.ascent = ascent;
        self.descent = descent;
        self.min_ele = min;
        self.max_ele = max;
        self.climbs = detect_climbs(self);
    }

    /// Replaces the elevation (e.g. sampled from a terrain model) and recomputes derived data.
    pub fn apply_elevation(&mut self, ele: Vec<f32>, smooth_m: f64) {
        self.ele = ele;
        self.has_elevation = true;
        self.finalize_elevation(smooth_m);
    }

    /// Geographic position → local (x, z) metres.
    pub fn to_local(&self, lat: f64, lon: f64) -> (f64, f64) {
        ((lon - self.lon0) * self.lat0.to_radians().cos() * M_PER_DEG, -(lat - self.lat0) * M_PER_DEG)
    }

    pub fn to_lat_lon(&self, x: f64, z: f64) -> (f64, f64) {
        (self.lat0 - z / M_PER_DEG, self.lon0 + x / (self.lat0.to_radians().cos() * M_PER_DEG))
    }

    pub fn sample_at(&self, dist: f64) -> RoutePos {
        let f = (dist / self.step).clamp(0.0, (self.n - 1) as f64);
        let i = (f.floor() as usize).min(self.n - 2);
        let t = f - i as f64;
        let lerp = |a: &[f64]| a[i] + (a[i + 1] - a[i]) * t;
        let lerp32 = |a: &[f32]| a[i] as f64 + (a[i + 1] - a[i]) as f64 * t;
        RoutePos { x: lerp(&self.x), z: lerp(&self.z), ele: lerp32(&self.ele), grade: lerp32(&self.grade), lat: lerp(&self.lat), lon: lerp(&self.lon) }
    }

    pub fn preview_outline(&self, max_points: usize) -> Vec<[f64; 2]> {
        let stride = self.n.div_ceil(max_points).max(1);
        let mut out: Vec<[f64; 2]> = (0..self.n).step_by(stride).map(|i| [self.lat[i], self.lon[i]]).collect();
        out.push([self.lat[self.n - 1], self.lon[self.n - 1]]);
        out
    }
}

fn cumulative(pts: &[RawPoint]) -> Vec<f64> {
    let mut cum = vec![0.0; pts.len()];
    for i in 1..pts.len() {
        cum[i] = cum[i - 1] + haversine(pts[i - 1].lat, pts[i - 1].lon, pts[i].lat, pts[i].lon);
    }
    cum
}

/// Walks a polyline at a fixed step, yielding `(segment index, fraction along it)`.
fn walk(cum: &[f64], n: usize, step: f64) -> impl Iterator<Item = (usize, f64)> + '_ {
    let mut j = 0;
    (0..n).map(move |i| {
        let target = i as f64 * step;
        while j + 2 < cum.len() && cum[j + 1] < target {
            j += 1;
        }
        (j, ((target - cum[j]) / (cum[j + 1] - cum[j])).clamp(0.0, 1.0))
    })
}

/// Rounds off the corners of the track. Planner exports and sparse GPS tracks
/// describe a hairpin as a few straight lines meeting at sharp angles, which no
/// road (or rider) actually follows.
fn round_corners(pts: &[RawPoint], cum: &[f64], step: f64) -> Vec<RawPoint> {
    let n = (cum[cum.len() - 1] / step).floor() as usize + 1;
    let mut out: Vec<RawPoint> = walk(cum, n, step)
        .map(|(j, t)| {
            let (a, b) = (pts[j], pts[j + 1]);
            let ele = match (a.ele, b.ele) {
                (Some(ea), Some(eb)) => Some(ea + (eb - ea) * t),
                (ea, eb) => ea.or(eb),
            };
            RawPoint { lat: a.lat + (b.lat - a.lat) * t, lon: a.lon + (b.lon - a.lon) * t, ele }
        })
        .collect();
    for _ in 0..2 {
        let prev = out.clone();
        for i in 0..n {
            // Shrink the window toward the ends so the start and finish stay put.
            let r = 2.min(i).min(n - 1 - i);
            let window = &prev[i - r..=i + r];
            out[i].lat = window.iter().map(|p| p.lat).sum::<f64>() / window.len() as f64;
            out[i].lon = window.iter().map(|p| p.lon).sum::<f64>() / window.len() as f64;
        }
    }
    // Keep the true end point, which the fixed step would otherwise cut short.
    out.push(pts[pts.len() - 1]);
    out
}

pub fn build_route(name: &str, raw: &[RawPoint]) -> Result<Route, String> {
    build_route_with_step(name, raw, 5.0)
}

pub fn build_route_with_step(name: &str, raw: &[RawPoint], step: f64) -> Result<Route, String> {
    // Drop stationary duplicates so every segment has a direction.
    let mut pts: Vec<RawPoint> = Vec::with_capacity(raw.len());
    for p in raw {
        if pts.last().is_none_or(|last| haversine(last.lat, last.lon, p.lat, p.lon) > 0.5) {
            pts.push(*p);
        }
    }
    if pts.len() < 2 {
        return Err("This route has no length.".into());
    }
    let first_cum = cumulative(&pts);
    if first_cum[pts.len() - 1] < 100.0 {
        return Err("This route is shorter than 100 m.".into());
    }
    let pts = round_corners(&pts, &first_cum, step);
    let cum = cumulative(&pts);
    let raw_total = cum[pts.len() - 1];

    // Fill gaps in elevation by linear interpolation between known neighbours.
    let has_elevation = pts.iter().any(|p| p.ele.is_some());
    let mut raw_ele = vec![0.0f64; pts.len()];
    if has_elevation {
        let mut prev: Option<usize> = None;
        for i in 0..pts.len() {
            let Some(e) = pts[i].ele else { continue };
            raw_ele[i] = e;
            match prev {
                None => raw_ele[..i].fill(e),
                Some(p) => {
                    for j in p + 1..i {
                        raw_ele[j] = raw_ele[p] + (e - raw_ele[p]) * (cum[j] - cum[p]) / (cum[i] - cum[p]);
                    }
                }
            }
            prev = Some(i);
        }
        let p = prev.unwrap();
        let last = raw_ele[p];
        raw_ele[p + 1..].fill(last);
    }

    let lat0 = pts.iter().map(|p| p.lat).sum::<f64>() / pts.len() as f64;
    let lon0 = pts.iter().map(|p| p.lon).sum::<f64>() / pts.len() as f64;
    let cos_lat = lat0.to_radians().cos();

    let n = (raw_total / step).floor() as usize + 1;
    let mut route = Route {
        name: name.to_string(),
        step,
        n,
        total: (n - 1) as f64 * step,
        lat0,
        lon0,
        x: Vec::with_capacity(n),
        z: Vec::with_capacity(n),
        ele: Vec::with_capacity(n),
        grade: vec![0.0; n],
        lat: Vec::with_capacity(n),
        lon: Vec::with_capacity(n),
        ascent: 0.0,
        descent: 0.0,
        min_ele: 0.0,
        max_ele: 0.0,
        climbs: Vec::new(),
        has_elevation,
        closed: false,
    };
    for (j, t) in walk(&cum, n, step) {
        let lat = pts[j].lat + (pts[j + 1].lat - pts[j].lat) * t;
        let lon = pts[j].lon + (pts[j + 1].lon - pts[j].lon) * t;
        route.lat.push(lat);
        route.lon.push(lon);
        route.x.push((lon - lon0) * cos_lat * M_PER_DEG);
        route.z.push(-(lat - lat0) * M_PER_DEG);
        route.ele.push((raw_ele[j] + (raw_ele[j + 1] - raw_ele[j]) * t) as f32);
    }
    route.finalize_elevation(30.0);
    Ok(route)
}

/// A synthetic ~15 km loop with one proper climb, so the app is usable before any GPX is imported.
pub fn demo_route() -> Route {
    const N: usize = 3000;
    let (lat0, lon0, radius) = (46.2f64, 7.6f64, 2200.0f64);
    let pts: Vec<RawPoint> = (0..=N)
        .map(|i| {
            let a = i as f64 / N as f64 * std::f64::consts::TAU;
            let r = radius * (1.0 + 0.22 * (3.0 * a).sin() + 0.08 * (7.0 * a).cos());
            RawPoint {
                lat: lat0 + r * a.sin() / M_PER_DEG,
                lon: lon0 + r * a.cos() / (M_PER_DEG * lat0.to_radians().cos()),
                ele: Some(620.0 + 110.0 * (a - 1.2).sin() + 14.0 * (4.0 * a).sin() + 3.0 * (9.0 * a).sin()),
            }
        })
        .collect();
    build_route("Demo Loop", &pts).expect("demo route is valid")
}
