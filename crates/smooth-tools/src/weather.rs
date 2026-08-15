//! `get_weather` — current conditions + a short forecast for a place.
//!
//! Keyless by design: geocoding and forecast both come from **Open-Meteo**
//! (`open-meteo.com`), which needs no API key or signup — so there is nothing
//! to manage in `@smooai/config`. When no location is given we fall back to the
//! daemon's own location via a keyless IP lookup (`ipapi.co`); for a personal
//! daemon running at home that resolves to the user's home town.
//!
//! Named `get_*` on purpose: the engine's permission classifier treats
//! `get_`/`read_`/`list_` tools as read-only-safe, so Auto mode never prompts
//! for it (same reasoning as `get_current_datetime`, pearl th-a78e1e). HTTP goes
//! through the house `smooai-fetch` client; a tool call runs in-process in the
//! daemon, so this is direct egress (not the goalie-gated `bash` path).

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooai_fetch::{Method, RequestInit};
use smooth_operator::{Tool, ToolSchema};

/// `get_weather` — what's the weather.
pub struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "get_weather".into(),
            description: "Get the CURRENT weather and a short (3-day) forecast for a place. Pass a city or place name (e.g. \"Fishers, IN\" or \"Tokyo\"); omit `location` to use where this machine is. Use it for anything about the weather, whether to bring a coat, will it rain, etc. Returns a compact human summary."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string", "description": "City/place name. Omit to use the daemon's own location." },
                    "units": { "type": "string", "enum": ["imperial", "metric"], "description": "imperial (°F, mph) — the default — or metric (°C, km/h)." }
                },
                "required": []
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let imperial = arguments.get("units").and_then(Value::as_str) != Some("metric");
        let place = arguments.get("location").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());

        let (lat, lon, label) = match place {
            Some(p) => geocode(p).await.ok_or_else(|| anyhow::anyhow!("couldn't find a place called `{p}`"))?,
            None => here()
                .await
                .ok_or_else(|| anyhow::anyhow!("no location given and couldn't detect one — pass a `location`"))?,
        };

        let (temp_unit, wind_unit) = if imperial { ("fahrenheit", "mph") } else { ("celsius", "kmh") };
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
             &current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m\
             &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
             &temperature_unit={temp_unit}&wind_speed_unit={wind_unit}&timezone=auto&forecast_days=3"
        );
        let data = get_json(&url)
            .await
            .ok_or_else(|| anyhow::anyhow!("weather service didn't respond for {label}"))?;
        Ok(render(&label, &data, imperial))
    }
}

/// Resolve a place name to `(lat, lon, label)` via Open-Meteo geocoding.
async fn geocode(place: &str) -> Option<(f64, f64, String)> {
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
        urlencode(place)
    );
    let hit = get_json(&url).await?;
    let r = hit.get("results")?.as_array()?.first()?;
    let lat = r.get("latitude")?.as_f64()?;
    let lon = r.get("longitude")?.as_f64()?;
    Some((lat, lon, place_label(r)))
}

/// A readable label from a geocoding result: "City, Region, CC" (dropping blanks).
fn place_label(r: &Value) -> String {
    let parts: Vec<String> = [
        r.get("name").and_then(Value::as_str),
        r.get("admin1").and_then(Value::as_str),
        r.get("country_code").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .map(str::to_string)
    .collect();
    if parts.is_empty() {
        "there".to_string()
    } else {
        parts.join(", ")
    }
}

/// How a device fix is labelled. CoreLocation hands back coordinates, not a
/// place name — reverse geocoding them would mean CLGeocoder (deprecated in the
/// SDK, "use MapKit") for a string nobody asked for.
const DEVICE_LABEL: &str = "your location";

/// Coordinates for "here" — the device's real position first, the IP guess
/// second (th-ecdf4d).
///
/// The IP lookup answers "where does this machine's traffic surface", which a
/// VPN moves to another country and a rural ISP moves to the nearest metro.
/// macOS Location Services answers "where is this Mac", so it wins whenever the
/// grant exists. Off macOS, or ungranted, the IP fallback is unchanged.
async fn here() -> Option<(f64, f64, String)> {
    let device = device_fix().await;
    // ponytail: only pay for the IP round-trip when the device had nothing.
    let ip = if device.is_none() { ip_locate().await } else { None };
    best(device, ip)
}

/// Device fix wins; otherwise the IP guess, label and all. Split out from
/// [`here`] so the preference order is testable without a TCC grant or a network.
fn best(device: Option<(f64, f64)>, ip: Option<(f64, f64, String)>) -> Option<(f64, f64, String)> {
    device.map(|(lat, lon)| (lat, lon, DEVICE_LABEL.to_owned())).or(ip)
}

/// The Mac's own position via macOS Location Services, or `None` when that isn't
/// available — no grant, no fix, or not a Mac at all.
async fn device_fix() -> Option<(f64, f64)> {
    #[cfg(target_os = "macos")]
    if let Ok(c) = crate::get_location::coordinates().await {
        return Some((c.lat, c.lon));
    }
    None
}

/// The daemon's own location via a keyless IP lookup — the fallback for [`here`].
async fn ip_locate() -> Option<(f64, f64, String)> {
    let v = get_json("https://ipapi.co/json/").await?;
    let lat = v.get("latitude")?.as_f64()?;
    let lon = v.get("longitude")?.as_f64()?;
    let label = [v.get("city").and_then(Value::as_str), v.get("region_code").and_then(Value::as_str)]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Some((lat, lon, if label.is_empty() { "your area".into() } else { label }))
}

/// GET a JSON document through the house resilient client; `None` on any failure.
async fn get_json(url: &str) -> Option<Value> {
    let mut headers = HashMap::new();
    headers.insert("accept".to_owned(), "application/json".to_owned());
    headers.insert(
        "user-agent".to_owned(),
        concat!("smooth-th/", env!("CARGO_PKG_VERSION"), " (https://smoo.ai)").to_owned(),
    );
    let init = RequestInit {
        method: Method::GET,
        headers,
        body: None,
    };
    match smooai_fetch::fetch::<Value>(url, init).await {
        Ok(resp) if resp.ok => resp.data,
        Ok(resp) => {
            tracing::debug!(url, status = resp.status, "weather source non-2xx");
            None
        }
        Err(err) => {
            tracing::debug!(url, %err, "weather source failed");
            None
        }
    }
}

/// WMO weather-interpretation code → (description, emoji). Codes per the
/// Open-Meteo docs. Unknown codes read as generic rather than blank.
fn wmo(code: i64) -> (&'static str, &'static str) {
    match code {
        0 => ("Clear", "☀️"),
        1 => ("Mainly clear", "🌤️"),
        2 => ("Partly cloudy", "⛅"),
        3 => ("Overcast", "☁️"),
        45 | 48 => ("Fog", "🌫️"),
        51 | 53 | 55 => ("Drizzle", "🌦️"),
        56..=57 => ("Freezing drizzle", "🌧️"),
        61 | 63 | 65 => ("Rain", "🌧️"),
        66..=67 => ("Freezing rain", "🌧️"),
        71 | 73 | 75 => ("Snow", "🌨️"),
        77 => ("Snow grains", "🌨️"),
        80..=82 => ("Rain showers", "🌦️"),
        85..=86 => ("Snow showers", "🌨️"),
        95 => ("Thunderstorm", "⛈️"),
        96 | 99 => ("Thunderstorm with hail", "⛈️"),
        _ => ("Unsettled", "🌡️"),
    }
}

/// Render the compact summary from an Open-Meteo forecast payload. Split from
/// `execute` so tests pin a fixed payload instead of hitting the network.
fn render(label: &str, data: &Value, imperial: bool) -> String {
    let t = if imperial { "°F" } else { "°C" };
    let w = if imperial { "mph" } else { "km/h" };
    let cur = &data["current"];
    let round = |v: &Value| v.as_f64().map(|n| n.round() as i64);

    let mut out = format!("Weather — {label}\n");
    if let Some(temp) = round(&cur["temperature_2m"]) {
        let (desc, emoji) = wmo(cur["weather_code"].as_i64().unwrap_or(-1));
        let feels = round(&cur["apparent_temperature"]);
        let hum = cur["relative_humidity_2m"].as_i64();
        let wind = round(&cur["wind_speed_10m"]);
        out.push_str(&format!("Now:   {temp}{t} {emoji} {desc}"));
        if let Some(f) = feels {
            if f != temp {
                out.push_str(&format!(" (feels {f}{t})"));
            }
        }
        if let Some(h) = hum {
            out.push_str(&format!(" · humidity {h}%"));
        }
        if let Some(ws) = wind {
            out.push_str(&format!(" · wind {ws} {w}"));
        }
        out.push('\n');
    }

    // Daily rows (arrays run parallel by index).
    let daily = &data["daily"];
    if let (Some(dates), Some(hi), Some(lo)) = (
        daily["time"].as_array(),
        daily["temperature_2m_max"].as_array(),
        daily["temperature_2m_min"].as_array(),
    ) {
        let codes = daily["weather_code"].as_array();
        let pop = daily["precipitation_probability_max"].as_array();
        for (i, date) in dates.iter().enumerate() {
            let day = match i {
                0 => "Today".to_string(),
                1 => "Tomorrow".to_string(),
                _ => date.as_str().unwrap_or("").to_string(),
            };
            let h = hi.get(i).and_then(Value::as_f64).map(|n| n.round() as i64);
            let l = lo.get(i).and_then(Value::as_f64).map(|n| n.round() as i64);
            let (Some(h), Some(l)) = (h, l) else { continue };
            let (desc, emoji) = wmo(codes.and_then(|c| c.get(i)).and_then(Value::as_i64).unwrap_or(-1));
            out.push_str(&format!("{day}: {h}{t} / {l}{t} {emoji} {desc}"));
            if let Some(p) = pop.and_then(|p| p.get(i)).and_then(Value::as_i64) {
                out.push_str(&format!(" · {p}% precip"));
            }
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Minimal percent-encoding for a place name in a query string. `ponytail`: a
/// hand-rolled encoder for the handful of chars a city name hits — not a crate.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "%20".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn wmo_maps_known_codes_and_falls_back() {
        assert_eq!(wmo(0).0, "Clear");
        assert_eq!(wmo(2).0, "Partly cloudy");
        assert_eq!(wmo(65).0, "Rain");
        assert_eq!(wmo(95).0, "Thunderstorm");
        assert_eq!(wmo(1234).0, "Unsettled", "unknown code falls back, never blank");
    }

    #[test]
    fn urlencode_handles_spaces_and_commas() {
        assert_eq!(urlencode("Fishers, IN"), "Fishers%2C%20IN");
        assert_eq!(urlencode("Tokyo"), "Tokyo");
    }

    #[test]
    fn place_label_joins_and_drops_blanks() {
        let r = json!({ "name": "Fishers", "admin1": "Indiana", "country_code": "US" });
        assert_eq!(place_label(&r), "Fishers, Indiana, US");
        let bare = json!({ "name": "Nowhere", "admin1": "", "country_code": "US" });
        assert_eq!(place_label(&bare), "Nowhere, US");
    }

    #[test]
    fn render_produces_current_and_forecast() {
        let data = json!({
            "current": { "temperature_2m": 72.4, "apparent_temperature": 70.1, "relative_humidity_2m": 55, "weather_code": 2, "wind_speed_10m": 8.3 },
            "daily": {
                "time": ["2026-08-15", "2026-08-16", "2026-08-17"],
                "weather_code": [2, 61, 0],
                "temperature_2m_max": [78.0, 74.2, 81.9],
                "temperature_2m_min": [61.1, 60.0, 64.5],
                "precipitation_probability_max": [20, 80, 5]
            }
        });
        let out = render("Fishers, Indiana, US", &data, true);
        assert!(out.contains("Weather — Fishers, Indiana, US"), "{out}");
        assert!(out.contains("Now:   72°F ⛅ Partly cloudy (feels 70°F) · humidity 55% · wind 8 mph"), "{out}");
        assert!(out.contains("Today: 78°F / 61°F ⛅ Partly cloudy · 20% precip"), "{out}");
        assert!(out.contains("Tomorrow: 74°F / 60°F 🌧️ Rain · 80% precip"), "{out}");
        // Day index 2 uses the date, not a relative word.
        assert!(out.contains("2026-08-17: 82°F / 65°F ☀️ Clear · 5% precip"), "{out}");
    }

    #[test]
    fn a_device_fix_beats_the_ip_guess() {
        // th-ecdf4d: the whole point — an IP guess two hours away must not win
        // over Location Services.
        let ip = Some((41.878, -87.629, "Chicago, IL".to_owned()));
        assert_eq!(best(Some((39.955, -86.013)), ip.clone()), Some((39.955, -86.013, DEVICE_LABEL.to_owned())));
        // No device fix (ungranted, or not a Mac) → the IP guess, unchanged.
        assert_eq!(best(None, ip.clone()), ip);
        // Neither → the caller's "pass a location" error, not a silent (0, 0).
        assert_eq!(best(None, None), None);
    }

    #[tokio::test]
    async fn an_ungranted_device_fix_is_none_rather_than_an_error() {
        // A test binary is never TCC-granted, so this is the ungranted path on
        // macOS and the always-path everywhere else. It must fall through to the
        // IP lookup instead of failing the whole weather call.
        if let Some((lat, lon)) = device_fix().await {
            // Granted on this machine — then at least it must be a real place.
            assert!((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon), "{lat},{lon}");
        }
    }

    #[test]
    fn render_metric_uses_celsius_kmh() {
        let data = json!({ "current": { "temperature_2m": 22.0, "apparent_temperature": 22.0, "weather_code": 0, "wind_speed_10m": 12.0 }, "daily": {} });
        let out = render("Tokyo", &data, false);
        assert!(out.contains("22°C"), "{out}");
        assert!(out.contains("wind 12 km/h"), "{out}");
        // feels == temp → no "(feels …)" noise.
        assert!(!out.contains("feels"), "{out}");
    }
}
