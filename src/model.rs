//! myUplink's systems, devices and data points, and how a data point maps
//! to an entity.
//!
//! A data point (`GET /v2/devices/{id}/points`) has `parameterId`,
//! `parameterName`, `parameterUnit`, `writable`, `value` (already scaled),
//! `minValue`, `maxValue` and `stepValue` (not scaled; multiply by
//! `scaleValue`) and `enumValues` for points with named values.

use serde_json::{json, Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub system_id: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub connected: bool,
    pub has_alarm: bool,
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// The devices in a `/v2/systems/me` answer. A system with one device names
/// the device after the system.
pub fn devices(systems: &Value) -> Vec<DeviceInfo> {
    let mut out = vec![];
    for s in systems["systems"].as_array().into_iter().flatten() {
        let system_id = text(&s["systemId"]).unwrap_or_default();
        let list = s["devices"].as_array().cloned().unwrap_or_default();
        for d in &list {
            let Some(id) = text(&d["id"]) else { continue };
            let product = text(&d["product"]["name"]);
            let (manufacturer, model) = match product.as_deref().and_then(|p| p.split_once(' ')) {
                Some((m, rest)) => (Some(m.to_string()), Some(rest.trim().to_string())),
                None => (None, product.clone()),
            };
            let name = if list.len() == 1 { text(&s["name"]) } else { None }.or(product.clone()).unwrap_or_else(|| id.clone());
            out.push(DeviceInfo {
                id,
                system_id: system_id.clone(),
                name,
                manufacturer,
                model,
                serial: text(&d["product"]["serialNumber"]),
                firmware: text(&d["currentFwVersion"]),
                connected: d["connectionState"].as_str().is_none_or(|c| c.eq_ignore_ascii_case("connected")),
                has_alarm: s["hasAlarm"].as_bool().unwrap_or(false),
            });
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A reading with a number.
    Number,
    /// A reading with named values.
    Enum(Vec<(String, String)>),
    /// A setting with a range.
    Setting { min: f64, max: f64, step: f64 },
    /// A setting with named values.
    Choice(Vec<(String, String)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub id: String,
    pub name: String,
    pub unit: String,
    pub category: String,
    pub kind: Kind,
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Names come with soft hyphens for line breaks in the app.
pub fn clean_name(s: &str) -> String {
    s.replace('\u{ad}', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn enums(p: &Value) -> Vec<(String, String)> {
    p["enumValues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some((id_of(&e["value"])?, text(&e["text"]).unwrap_or_else(|| id_of(&e["value"]).unwrap_or_default()))))
        .collect()
}

pub fn parse_point(p: &Value) -> Option<Point> {
    let id = id_of(&p["parameterId"])?;
    let name = clean_name(p["parameterName"].as_str().unwrap_or(&id));
    let unit = p["parameterUnit"].as_str().unwrap_or("").trim().to_string();
    let writable = p["writable"].as_bool().unwrap_or(false);
    let scale = number(&p["scaleValue"]).filter(|s| *s > 0.0).unwrap_or(1.0);
    let e = enums(p);
    let kind = match (writable, e.is_empty()) {
        (true, false) => Kind::Choice(e),
        (false, false) => Kind::Enum(e),
        (true, true) => {
            let min = number(&p["minValue"]).map(|v| v * scale).unwrap_or(-3000.0);
            let max = number(&p["maxValue"]).map(|v| v * scale).unwrap_or(3000.0);
            let step = number(&p["stepValue"]).map(|v| v * scale).filter(|s| *s > 0.0).unwrap_or(scale);
            Kind::Setting { min, max, step: round(step) }
        }
        (false, true) => Kind::Number,
    };
    Some(Point { id, name, unit, category: p["category"].as_str().unwrap_or("").to_string(), kind })
}

fn round(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// The points of a device by id, with their raw values.
pub fn values(points: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    for p in points.as_array().into_iter().flatten() {
        if let Some(id) = id_of(&p["parameterId"]) {
            m.insert(id, p["value"].clone());
        }
    }
    m
}

/// The entity domain for a point.
pub fn domain(p: &Point) -> &'static str {
    match p.kind {
        Kind::Number | Kind::Enum(_) => "sensor",
        Kind::Setting { .. } => "number",
        Kind::Choice(_) => "select",
    }
}

/// Device class, state class and unit for a numeric point by its unit.
pub fn classes(unit: &str) -> (Option<&'static str>, Option<&'static str>, Option<String>) {
    let u = unit.trim();
    let m = Some("measurement");
    match u {
        "°C" | "°F" => (Some("temperature"), m, Some(u.into())),
        "kW" | "W" => (Some("power"), m, Some(u.into())),
        "kWh" | "Wh" | "MWh" => (Some("energy"), Some("total_increasing"), Some(u.into())),
        "A" => (Some("current"), m, Some(u.into())),
        "V" => (Some("voltage"), m, Some(u.into())),
        "Hz" => (Some("frequency"), m, Some(u.into())),
        "bar" | "kPa" => (Some("pressure"), m, Some(u.into())),
        "h" => (Some("duration"), Some("total_increasing"), Some(u.into())),
        "l/m" | "l/min" => (Some("volume_flow_rate"), m, Some("L/min".into())),
        "" => (None, None, None),
        _ => (None, m, Some(u.into())),
    }
}

fn label(options: &[(String, String)], v: &Value) -> Option<String> {
    let key = id_of(v).or_else(|| number(v).map(|n| if n.fract() == 0.0 { format!("{}", n as i64) } else { n.to_string() }))?;
    options.iter().find(|(k, _)| *k == key).map(|(_, t)| t.clone()).or(Some(key))
}

/// Option texts, made unique so a select can tell them apart.
pub fn option_texts(options: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (k, t) in options {
        if out.contains(t) {
            out.push(format!("{t} ({k})"));
        } else {
            out.push(t.clone());
        }
    }
    out
}

/// The state and attributes of a point's entity.
pub fn state(p: &Point, value: &Value) -> (String, Map<String, Value>) {
    let mut a = Map::new();
    let s = match &p.kind {
        Kind::Number => number(value).map(|n| round(n).to_string()),
        Kind::Enum(o) => label(o, value),
        Kind::Choice(o) => {
            let texts = option_texts(o);
            a.insert("options".into(), json!(texts));
            let key = id_of(value).or_else(|| number(value).map(|n| format!("{}", n as i64)));
            key.and_then(|k| o.iter().position(|(v, _)| *v == k)).map(|i| texts[i].clone())
        }
        Kind::Setting { min, max, step } => {
            a.insert("min".into(), json!(min));
            a.insert("max".into(), json!(max));
            a.insert("step".into(), json!(step));
            a.insert("mode".into(), json!("box"));
            number(value).map(|n| round(n).to_string())
        }
    };
    (s.unwrap_or_else(|| "unknown".into()), a)
}

/// The value to write for a service call on a point's entity.
pub fn write_value(p: &Point, service: &str, data: &Map<String, Value>) -> Result<Value, String> {
    match (&p.kind, service) {
        (Kind::Setting { min, max, step }, "set_value") => {
            let v = data.get("value").and_then(number).ok_or("value mangler")?;
            if v < *min - 1e-9 || v > *max + 1e-9 {
                return Err(format!("{v} er uden for {min} til {max}"));
            }
            let v = round(((v - min) / step).round() * step + min);
            Ok(json!(v))
        }
        (Kind::Choice(o), "select_option") => {
            let opt = data.get("option").and_then(Value::as_str).ok_or("option mangler")?;
            let texts = option_texts(o);
            let i = texts.iter().position(|t| t == opt).ok_or_else(|| format!("ukendt valg: {opt}"))?;
            let key = &o[i].0;
            Ok(key.parse::<i64>().map(|n| json!(n)).unwrap_or_else(|_| json!(key)))
        }
        _ => Err(String::new()),
    }
}
