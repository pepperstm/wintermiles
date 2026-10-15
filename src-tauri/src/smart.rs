//! Fan and lights: a smart plug, Home Assistant or any webhook, driven by speed,
//! power or heart rate during a ride.

use std::path::Path;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use wintermiles_core::fitness::power_zone;
use wintermiles_core::types::SmartHome;

use crate::secrets;

/// Zone colours (1–7) for a light, as RGB.
const ZONE_RGB: [[u8; 3]; 7] = [[138, 148, 166], [74, 163, 255], [77, 219, 138], [255, 210, 74], [255, 154, 60], [255, 90, 74], [212, 74, 255]];

#[derive(Clone, Copy, PartialEq)]
struct Wanted {
    /// Fan level 0–100 in steps of 25.
    fan: u8,
    zone: u8,
}

pub struct SmartLink {
    tx: mpsc::UnboundedSender<Option<(Wanted, f64, f64, Option<f64>)>>,
    cfg: SmartHome,
}

/// Fan level for a reading between `from` (off) and `full`, in steps of 25%.
pub fn fan_level(value: f64, from: f64, full: f64) -> u8 {
    if value < from || full <= from {
        return if value >= full && full > 0.0 { 100 } else { 0 };
    }
    let f = ((value - from) / (full - from)).clamp(0.0, 1.0);
    (((f * 3.0).ceil() + 1.0) * 25.0).min(100.0) as u8
}

impl SmartLink {
    pub fn start(dir: &Path, cfg: SmartHome) -> Option<SmartLink> {
        if cfg.kind == "none" || cfg.url.trim().is_empty() {
            return None;
        }
        let token = secrets::load_named(dir, "homeassistant").unwrap_or_default();
        let (tx, mut rx) = mpsc::unbounded_channel::<Option<(Wanted, f64, f64, Option<f64>)>>();
        let c = cfg.clone();
        tauri::async_runtime::spawn(async move {
            let http = reqwest::Client::builder().timeout(Duration::from_secs(4)).build().unwrap_or_default();
            let mut sent: Option<Wanted> = None;
            let mut last_change = Instant::now() - Duration::from_secs(60);
            while let Some(msg) = rx.recv().await {
                let Some((want, speed, power, hr)) = msg else {
                    // Ride over: fan and light off.
                    let _ = apply(&http, &c, &token, Wanted { fan: 0, zone: 0 }, 0.0, 0.0, None).await;
                    break;
                };
                // Don't flap: change at most every 8 seconds.
                if sent == Some(want) || last_change.elapsed() < Duration::from_secs(8) {
                    continue;
                }
                if let Err(e) = apply(&http, &c, &token, want, speed, power, hr).await {
                    eprintln!("Fan/lights: {e}");
                }
                sent = Some(want);
                last_change = Instant::now();
            }
        });
        Some(SmartLink { tx, cfg })
    }

    /// Called about once a second with the latest readings.
    pub fn update(&self, speed_kph: f64, power: f64, hr: Option<f64>, ftp: f64) {
        let value = match self.cfg.fan_by.as_str() {
            "power" => power,
            "hr" => hr.unwrap_or(0.0),
            _ => speed_kph,
        };
        let want = Wanted { fan: fan_level(value, self.cfg.fan_from, self.cfg.fan_full), zone: if power > 5.0 { power_zone(power, ftp) } else { 0 } };
        let _ = self.tx.send(Some((want, speed_kph, power, hr)));
    }

    pub fn stop(self) {
        let _ = self.tx.send(None);
    }
}

async fn ok(res: Result<reqwest::Response, reqwest::Error>) -> Result<(), String> {
    match res {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!("the device answered {}", r.status())),
        Err(e) => Err(format!("could not reach the device: {e}")),
    }
}

fn base(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

async fn apply(http: &reqwest::Client, c: &SmartHome, token: &str, w: Wanted, speed: f64, power: f64, hr: Option<f64>) -> Result<(), String> {
    let on = w.fan > 0;
    match c.kind.as_str() {
        "homeassistant" => {
            let call = |domain: &str, service: &str, body: serde_json::Value| http.post(format!("{}/api/services/{domain}/{service}", base(&c.url))).bearer_auth(token).json(&body).send();
            if !c.fan_entity.is_empty() {
                let e = &c.fan_entity;
                if e.starts_with("fan.") {
                    ok(call("fan", "set_percentage", serde_json::json!({ "entity_id": e, "percentage": w.fan })).await).await?;
                } else {
                    let domain = e.split('.').next().unwrap_or("switch");
                    ok(call(domain, if on { "turn_on" } else { "turn_off" }, serde_json::json!({ "entity_id": e })).await).await?;
                }
            }
            if !c.light_entity.is_empty() {
                let e = &c.light_entity;
                if w.zone == 0 {
                    ok(call("light", "turn_off", serde_json::json!({ "entity_id": e })).await).await?;
                } else {
                    ok(call("light", "turn_on", serde_json::json!({ "entity_id": e, "rgb_color": ZONE_RGB[(w.zone - 1) as usize], "brightness": 255 })).await).await?;
                }
            }
            Ok(())
        }
        "shelly" => {
            // Gen 2+ first, then the Gen 1 API.
            let gen2 = ok(http.get(format!("{}/rpc/Switch.Set?id=0&on={on}", base(&c.url))).send().await).await;
            if gen2.is_ok() {
                return Ok(());
            }
            ok(http.get(format!("{}/relay/0?turn={}", base(&c.url), if on { "on" } else { "off" })).send().await).await
        }
        "tasmota" => {
            let cmd = if on { "Power%20On" } else { "Power%20Off" };
            ok(http.get(format!("{}/cm?cmnd={cmd}", base(&c.url))).send().await).await?;
            if on {
                // Fans on a dimmer or fan module take a level too; plain plugs ignore it.
                let _ = http.get(format!("{}/cm?cmnd=Dimmer%20{}", base(&c.url), w.fan)).send().await;
            }
            Ok(())
        }
        "webhook" => {
            let url = c
                .url
                .replace("{level}", &w.fan.to_string())
                .replace("{zone}", &w.zone.to_string())
                .replace("{power}", &format!("{power:.0}"))
                .replace("{speed}", &format!("{speed:.1}"))
                .replace("{hr}", &hr.map_or(String::new(), |h| format!("{h:.0}")));
            ok(http.get(base(&url)).send().await).await
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn fan_levels() {
        assert_eq!(fan_level(5.0, 10.0, 35.0), 0);
        assert_eq!(fan_level(10.0, 10.0, 35.0), 25);
        assert_eq!(fan_level(20.0, 10.0, 35.0), 75);
        assert_eq!(fan_level(35.0, 10.0, 35.0), 100);
        assert_eq!(fan_level(60.0, 10.0, 35.0), 100);
    }

    /// A pretend device that records the paths it is asked for.
    async fn device() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(format!("{line} {body}"));
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
            }
        });
        (format!("127.0.0.1:{}", addr.port()), seen)
    }

    #[tokio::test]
    async fn drives_a_webhook_and_home_assistant() {
        let (addr, seen) = device().await;
        let http = reqwest::Client::new();
        let hook = SmartHome { kind: "webhook".into(), url: format!("http://{addr}/fan?level={{level}}&zone={{zone}}&p={{power}}"), ..Default::default() };
        apply(&http, &hook, "", Wanted { fan: 75, zone: 4 }, 25.0, 210.0, None).await.unwrap();
        let ha = SmartHome { kind: "homeassistant".into(), url: format!("http://{addr}"), fan_entity: "fan.desk".into(), light_entity: "light.room".into(), ..Default::default() };
        apply(&http, &ha, "token", Wanted { fan: 50, zone: 6 }, 25.0, 300.0, None).await.unwrap();
        let shelly = SmartHome { kind: "shelly".into(), url: addr.clone(), ..Default::default() };
        apply(&http, &shelly, "", Wanted { fan: 0, zone: 0 }, 0.0, 0.0, None).await.unwrap();
        let log = seen.lock().unwrap().clone();
        assert!(log[0].starts_with("GET /fan?level=75&zone=4&p=210 "), "{log:?}");
        assert!(log[1].starts_with("POST /api/services/fan/set_percentage") && log[1].contains("\"percentage\":50"), "{log:?}");
        assert!(log[2].starts_with("POST /api/services/light/turn_on") && log[2].contains("[255,90,74]"), "{log:?}");
        assert!(log[3].starts_with("GET /rpc/Switch.Set?id=0&on=false"), "{log:?}");
    }
}
