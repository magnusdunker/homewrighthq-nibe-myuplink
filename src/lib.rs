//! myUplink: NIBE heat pumps (and other brands on myUplink) through the
//! myUplink cloud, as an external integration for HomewrightHQ.
//!
//! The user registers an application at dev.myuplink.com and enters its
//! client id and secret. By default the integration logs in with the client
//! credentials grant, which needs no browser. The authorization code grant
//! is offered as well; then the user pastes the redirect address back into
//! the form, because the plugin protocol cannot receive it.
//!
//! Every data point of a device becomes an entity: readings are sensors,
//! writable points with a range are numbers, and writable points with named
//! values are selects. Writing needs a myUplink Premium subscription.

pub mod api;
pub mod model;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use homewright_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ApiError, Client, Grant, Urls};
use model::{DeviceInfo, Point};

pub const DOMAIN: &str = "myuplink";
pub const DEFAULT_SCAN_MINUTES: u64 = 5;
const MIN_SCAN_MINUTES: u64 = 1;
const MAX_SCAN_MINUTES: u64 = 60;
/// How often the system list (alarms, connection) is read.
const SYSTEMS_POLL: Duration = Duration::from_secs(30 * 60);
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_REDIRECT: &str = "http://localhost:8123/auth/external/callback";

/// Logging to HomewrightHQ's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    homewright_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

const SETUP_TEXT: &str = "myUplink kræver, at du opretter din egen applikation:

1. Log ind på dev.myuplink.com med den konto, du bruger i myUplink-appen.
2. Opret en ny applikation under »Applications«. Som callback URL skriver du:
   {redirect_uri}
3. Kopiér applikationens Client Identifier og Client Secret ind nedenfor.

Med »Client credentials« logger HomewrightHQ ind som applikationen og ser de anlæg, din egen konto har adgang til. Virker det ikke, så vælg »Login i browseren«; så skal callback-adressen stå præcis ens her og på dev.myuplink.com.";

const AUTH_TEXT: &str = "Åbn denne adresse, log ind med din myUplink-konto og giv applikationen adgang:

{url}

myUplink sender derefter browseren videre til callback-adressen. Siden dér kan vise en fejl eller »Linket er udløbet«; det gør ikke noget. Kopiér hele adressen fra browserens adresselinje (den indeholder code=...) og indsæt den nedenfor.";

fn user_form(d: &Map<String, Value>, errors: Value) -> FlowReply {
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let redirect = Some(s("redirect_uri")).filter(|r| !r.is_empty()).unwrap_or_else(|| DEFAULT_REDIRECT.into());
    let grant = Some(s("grant")).filter(|g| !g.is_empty()).unwrap_or_else(|| "client_credentials".into());
    let schema = json!({
        "type": "object",
        "required": ["client_id", "client_secret"],
        "properties": {
            "client_id": { "type": "string", "title": "Client Identifier", "default": s("client_id") },
            "client_secret": { "type": "string", "format": "password", "title": "Client Secret" },
            "grant": {
                "type": "string", "title": "Login", "default": grant,
                "oneOf": [
                    { "const": "client_credentials", "title": "Client credentials (uden browser)" },
                    { "const": "authorization_code", "title": "Login i browseren" }
                ]
            },
            "redirect_uri": { "type": "string", "title": "Callback URL", "default": redirect, "description": "Kun ved login i browseren. Skal stå præcis sådan på dev.myuplink.com" }
        }
    });
    let mut r = FlowReply::form_with_errors("user", schema, errors);
    r.result["description"] = json!(SETUP_TEXT);
    r.result["description_placeholders"] = json!({ "redirect_uri": redirect });
    r
}

fn auth_form(url: &str, errors: Value) -> FlowReply {
    let schema = json!({
        "type": "object",
        "required": ["redirect_url"],
        "properties": {
            "redirect_url": { "type": "string", "title": "Adressen, myUplink sendte dig til", "description": "Hele adressen fra adresselinjen, eller kun koden efter code=" }
        }
    });
    let mut r = FlowReply::form_with_errors("auth", schema, errors);
    r.result["description"] = json!(AUTH_TEXT);
    r.result["description_placeholders"] = json!({ "url": url });
    r
}

fn options_schema(minutes: u64) -> Value {
    json!({
        "type": "object",
        "properties": {
            "scan_interval": {
                "type": "integer", "title": "Minutter mellem opdateringer", "default": minutes,
                "minimum": MIN_SCAN_MINUTES, "maximum": MAX_SCAN_MINUTES
            }
        }
    })
}

fn urls(get: impl Fn(&str) -> Option<String>) -> Urls {
    let d = Urls::default();
    Urls { authorize: get("authorize_url").unwrap_or(d.authorize), token: get("token_url").unwrap_or(d.token), api: get("api_url").unwrap_or(d.api) }
}

const URL_KEYS: [&str; 3] = ["authorize_url", "token_url", "api_url"];

pub fn scan_interval(entry: &Entry) -> Duration {
    let m = entry.get("scan_interval").and_then(Value::as_u64).unwrap_or(DEFAULT_SCAN_MINUTES).clamp(MIN_SCAN_MINUTES, MAX_SCAN_MINUTES);
    Duration::from_secs(m * 60)
}

fn str_of(m: &Map<String, Value>, k: &str) -> String {
    m.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

#[derive(Default)]
pub struct MyUplink {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
}

impl MyUplink {
    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }
}

/// Check the login by listing the systems, and make the entry.
async fn finish(flow: &Flow, d: &Map<String, Value>, client: Client, refresh: Option<String>) -> Result<FlowReply, &'static str> {
    let systems = match client.systems().await {
        Ok(s) => s,
        Err(e) => {
            info!("{e}");
            return Err(if matches!(e, ApiError::Auth(_)) { "invalid_auth" } else { "cannot_connect" });
        }
    };
    let entry_data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
    let mut data = if flow.source == "reauth" { entry_data } else { Map::new() };
    for k in ["client_id", "client_secret", "grant", "redirect_uri"].into_iter().chain(URL_KEYS) {
        if let Some(x) = d.get(k) {
            data.insert(k.into(), x.clone());
        }
    }
    if let Some(r) = client.take_new_refresh().or(refresh) {
        data.insert("refresh_token".into(), json!(r));
    }
    if flow.source == "reauth" {
        return Ok(FlowReply::create("", Value::Object(data)));
    }
    if model::devices(&systems).is_empty() {
        return Ok(FlowReply::abort("no_devices_found"));
    }
    let title = systems["systems"][0]["name"].as_str().map(|n| format!("myUplink ({n})")).unwrap_or_else(|| "myUplink".into());
    Ok(FlowReply::create(&title, Value::Object(data)).with_unique_id(str_of(d, "client_id")))
}

#[async_trait]
impl Integration for MyUplink {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        match (step, input) {
            ("user", None) => Ok(user_form(&flow.data, json!({}))),
            ("reauth", None) => {
                let mut d = flow.data.clone();
                for (k, v) in flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default() {
                    d.entry(k).or_insert(v);
                }
                Ok(user_form(&d, json!({})).with_data(d))
            }
            ("user", Some(v)) => {
                let mut d = flow.data.clone();
                for k in ["client_id", "client_secret", "grant", "redirect_uri"] {
                    d.insert(k.into(), json!(v[k].as_str().unwrap_or("").trim()));
                }
                if str_of(&d, "client_id").is_empty() || str_of(&d, "client_secret").is_empty() {
                    return Ok(user_form(&d, json!({ "base": "invalid_client" })).with_data(d));
                }
                let u = urls(|k| d.get(k).and_then(Value::as_str).map(String::from));
                let (id, secret) = (str_of(&d, "client_id"), str_of(&d, "client_secret"));
                match Grant::parse(&str_of(&d, "grant")) {
                    Grant::ClientCredentials => {
                        d.insert("grant".into(), json!("client_credentials"));
                        let tokens = match api::client_credentials(&api::http(), &u, &id, &secret).await {
                            Ok(t) => t,
                            Err(e) => {
                                info!("{e}");
                                let code = if matches!(e, ApiError::Auth(_)) { "invalid_client" } else { "cannot_connect" };
                                return Ok(user_form(&d, json!({ "base": code })).with_data(d));
                            }
                        };
                        let client = Client::new(u, Grant::ClientCredentials, &id, &secret, "", Some(tokens));
                        match finish(&flow, &d, client, None).await {
                            Ok(r) => Ok(r),
                            Err(code) => Ok(user_form(&d, json!({ "base": code })).with_data(d)),
                        }
                    }
                    Grant::AuthorizationCode => {
                        if reqwest::Url::parse(&str_of(&d, "redirect_uri")).is_err() {
                            return Ok(user_form(&d, json!({ "base": "invalid_redirect" })).with_data(d));
                        }
                        let state = api::new_state();
                        let url = api::authorize_url(&u, &id, &str_of(&d, "redirect_uri"), &state);
                        d.insert("oauth_state".into(), json!(state));
                        d.insert("authorize".into(), json!(url));
                        Ok(auth_form(&url, json!({})).with_data(d))
                    }
                }
            }
            ("auth", Some(v)) => {
                let d = flow.data.clone();
                let url = str_of(&d, "authorize");
                let code = match api::parse_redirect(v["redirect_url"].as_str().unwrap_or(""), &str_of(&d, "oauth_state")) {
                    Ok(c) => c,
                    Err(e) => return Ok(auth_form(&url, json!({ "base": e })).with_data(d)),
                };
                let u = urls(|k| d.get(k).and_then(Value::as_str).map(String::from));
                let (id, secret) = (str_of(&d, "client_id"), str_of(&d, "client_secret"));
                let tokens = match api::exchange_code(&api::http(), &u, &id, &secret, &str_of(&d, "redirect_uri"), &code).await {
                    Ok(t) => t,
                    Err(e) => {
                        info!("{e}");
                        let code = if matches!(e, ApiError::Auth(_)) { "invalid_grant" } else { "cannot_connect" };
                        return Ok(auth_form(&url, json!({ "base": code })).with_data(d));
                    }
                };
                let Some(refresh) = tokens.refresh_token.clone() else {
                    return Ok(auth_form(&url, json!({ "base": "no_refresh_token" })).with_data(d));
                };
                let client = Client::new(u, Grant::AuthorizationCode, &id, &secret, &refresh, Some(tokens));
                match finish(&flow, &d, client, Some(refresh)).await {
                    Ok(r) => Ok(r),
                    Err(code) => Ok(auth_form(&url, json!({ "base": code })).with_data(d)),
                }
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, _step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let current = flow.entry.as_ref().map(|e| scan_interval(e).as_secs() / 60).unwrap_or(DEFAULT_SCAN_MINUTES);
        match input {
            None => Ok(FlowReply::form("init", options_schema(current))),
            Some(v) => {
                let m = v["scan_interval"].as_u64().unwrap_or(current).clamp(MIN_SCAN_MINUTES, MAX_SCAN_MINUTES);
                Ok(FlowReply::create("", json!({ "scan_interval": m })))
            }
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let data = entry.data.as_object().cloned().unwrap_or_default();
        let grant = Grant::parse(&str_of(&data, "grant"));
        let refresh = str_of(&data, "refresh_token");
        if grant == Grant::AuthorizationCode && refresh.is_empty() {
            return Err(Error::auth_failed("intet refresh token; log ind igen"));
        }
        let client = Arc::new(Client::new(urls(|k| entry.get_str(k).map(String::from)), grant, &str_of(&data, "client_id"), &str_of(&data, "client_secret"), &refresh, None));
        let account = Arc::new(Account {
            entry_id: entry_id.clone(),
            host: host.clone(),
            client: client.clone(),
            data: Mutex::new(data),
            devices: Mutex::new(HashMap::new()),
            entities: Mutex::new(vec![]),
            task: Mutex::new(None),
            paused_until: Mutex::new(None),
        });
        let systems = client.systems().await;
        account.persist().await;
        let systems = match systems {
            Ok(s) => s,
            Err(ApiError::Auth(e)) => return Err(Error::auth_failed(format!("myUplink afviste login: {e}"))),
            Err(e) => return Err(Error::not_ready(e)),
        };
        let _ = host.delete_issue(&entry_id, "auth").await;
        for info in model::devices(&systems) {
            let points = client.points(&info.id).await.map_err(Error::not_ready)?;
            account.add_device(info, &points).await?;
        }
        account.persist().await;
        account.publish(None).await;

        let interval = scan_interval(&entry);
        let acc = account.clone();
        let task = tokio::spawn(async move {
            let mut last_systems = Instant::now();
            loop {
                tokio::time::sleep(interval).await;
                let systems_due = last_systems.elapsed() >= SYSTEMS_POLL;
                acc.poll(systems_due).await;
                if systems_due {
                    last_systems = Instant::now();
                }
            }
        });
        *account.task.lock().unwrap() = Some(task);
        if let Some(old) = self.accounts.lock().unwrap().insert(entry_id, account) {
            old.stop();
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, data: Map<String, Value>) -> Result<(), Error> {
        self.account(entry_id)?.call_service(entity_id, service, &data).await
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = entry.data.clone();
        if let Value::Object(m) = &mut d {
            for k in ["client_secret", "refresh_token"] {
                if m.contains_key(k) {
                    m.insert(k.into(), json!("**REDACTED**"));
                }
            }
        }
        let devices: Vec<Value> = self
            .accounts
            .lock()
            .unwrap()
            .get(&entry.entry_id)
            .map(|a| {
                a.devices
                    .lock()
                    .unwrap()
                    .values()
                    .map(|dev| json!({ "model": dev.info.model, "firmware": dev.info.firmware, "connected": dev.info.connected, "values": dev.values }))
                    .collect()
            })
            .unwrap_or_default();
        json!({ "entry": d, "devices": devices })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Point(String),
    Alarm,
}

struct Dev {
    info: DeviceInfo,
    device_id: String,
    points: HashMap<String, Point>,
    values: Map<String, Value>,
    available: bool,
}

type StateUpdate = (String, Map<String, Value>);

struct Account {
    entry_id: String,
    host: Host,
    client: Arc<Client>,
    data: Mutex<Map<String, Value>>,
    devices: Mutex<HashMap<String, Dev>>,
    /// (entity_id, device id, kind)
    entities: Mutex<Vec<(String, String, Kind)>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    paused_until: Mutex<Option<Instant>>,
}

impl Account {
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }

    async fn persist(&self) {
        let Some(r) = self.client.take_new_refresh() else { return };
        let data = {
            let mut d = self.data.lock().unwrap();
            d.insert("refresh_token".into(), json!(r));
            d.clone()
        };
        if let Err(e) = self.host.update_entry_data(&self.entry_id, Value::Object(data)).await {
            warn!("kunne ikke gemme det nye refresh token: {e}");
        }
    }

    async fn add_device(&self, info: DeviceInfo, points: &Value) -> Result<(), Error> {
        let dev = self
            .host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), info.id.clone())],
                    name: info.name.clone(),
                    manufacturer: info.manufacturer.clone(),
                    model: info.model.clone(),
                    serial_number: info.serial.clone(),
                    sw_version: info.firmware.clone(),
                    ..Default::default()
                },
            )
            .await?;
        let alarm = Entity::new("binary_sensor", format!("{}-alarm", info.id)).name("Alarm").device(&dev).device_class("problem");
        if let Some(entity_id) = self.host.add_entity(&self.entry_id, alarm, false).await? {
            self.entities.lock().unwrap().push((entity_id, info.id.clone(), Kind::Alarm));
        }
        let id = info.id.clone();
        self.devices.lock().unwrap().insert(
            id.clone(),
            Dev { available: info.connected, info, device_id: dev, points: HashMap::new(), values: model::values(points) },
        );
        self.add_points(&id, points).await
    }

    /// Entities for points not seen before.
    async fn add_points(&self, id: &str, points: &Value) -> Result<(), Error> {
        let (dev, new): (String, Vec<Point>) = {
            let devices = self.devices.lock().unwrap();
            let Some(d) = devices.get(id) else { return Ok(()) };
            let new = points.as_array().into_iter().flatten().filter_map(model::parse_point).filter(|p| !d.points.contains_key(&p.id)).collect();
            (d.device_id.clone(), new)
        };
        for p in new {
            let mut e = Entity::new(model::domain(&p), format!("{id}-{}", p.id)).name(&p.name).device(&dev);
            match &p.kind {
                model::Kind::Number => {
                    let (class, state_class, unit) = model::classes(&p.unit);
                    e.device_class = class.map(String::from);
                    e.state_class = state_class.map(String::from);
                    e.unit_of_measurement = unit;
                }
                model::Kind::Enum(o) => {
                    e = e.device_class("enum").capabilities(json!({ "options": model::option_texts(o) }));
                }
                model::Kind::Setting { min, max, step } => {
                    let (class, _, unit) = model::classes(&p.unit);
                    e.device_class = class.filter(|c| *c == "temperature").map(String::from);
                    e.unit_of_measurement = unit;
                    e = e.category("config").capabilities(json!({ "min": min, "max": max, "step": step, "mode": "box" }));
                }
                model::Kind::Choice(o) => {
                    e = e.category("config").capabilities(json!({ "options": model::option_texts(o) }));
                }
            }
            let handler = matches!(p.kind, model::Kind::Setting { .. } | model::Kind::Choice(_));
            let entity_id = self.host.add_entity(&self.entry_id, e, handler).await?;
            if let Some(d) = self.devices.lock().unwrap().get_mut(id) {
                if let Some(entity_id) = entity_id {
                    self.entities.lock().unwrap().push((entity_id, id.to_string(), Kind::Point(p.id.clone())));
                }
                d.points.insert(p.id.clone(), p);
            }
        }
        Ok(())
    }

    /// Push states for one device, or all.
    async fn publish(&self, only: Option<&str>) {
        let updates: Vec<(String, Option<StateUpdate>)> = {
            let devices = self.devices.lock().unwrap();
            self.entities
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, d, _)| only.is_none_or(|o| o == d))
                .map(|(entity_id, id, kind)| {
                    let Some(d) = devices.get(id).filter(|d| d.available) else { return (entity_id.clone(), None) };
                    let v = match kind {
                        Kind::Alarm => Some(((if d.info.has_alarm { "on" } else { "off" }).to_string(), Map::new())),
                        Kind::Point(pid) => d.points.get(pid).map(|p| model::state(p, d.values.get(pid).unwrap_or(&Value::Null))),
                    };
                    (entity_id.clone(), v)
                })
                .collect()
        };
        for (entity_id, v) in updates {
            let res = match v {
                Some((state, attrs)) => self.host.set_state(&entity_id, state, attrs).await,
                None => self.host.set_unavailable(&entity_id).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {entity_id}: {e}");
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused_until.lock().unwrap().is_some_and(|t| Instant::now() < t)
    }

    async fn poll(&self, systems_due: bool) {
        if self.paused() {
            return;
        }
        if systems_due {
            match self.client.systems().await {
                Ok(s) => {
                    let mut devices = self.devices.lock().unwrap();
                    for info in model::devices(&s) {
                        if let Some(d) = devices.get_mut(&info.id) {
                            d.info = info;
                        }
                    }
                }
                Err(e) => self.report(&e).await,
            }
        }
        let ids: Vec<String> = self.devices.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let r = self.client.points(&id).await;
            self.persist().await;
            match r {
                Ok(points) => {
                    let _ = self.host.delete_issue(&self.entry_id, "auth").await;
                    if let Err(e) = self.add_points(&id, &points).await {
                        warn!("nye datapunkter: {e}");
                    }
                    if let Some(d) = self.devices.lock().unwrap().get_mut(&id) {
                        d.values = model::values(&points);
                        d.available = d.info.connected;
                    }
                }
                Err(e) => {
                    let limited = matches!(e, ApiError::RateLimited(_));
                    self.report(&e).await;
                    if limited {
                        return;
                    }
                    if let Some(d) = self.devices.lock().unwrap().get_mut(&id) {
                        d.available = false;
                    }
                }
            }
            self.publish(Some(&id)).await;
        }
    }

    async fn report(&self, e: &ApiError) {
        let issue = match e {
            ApiError::Auth(m) => {
                warn!("{m}");
                Some(("auth", "myUplink: log ind igen", "myUplink godtager ikke længere integrationens login. Godkend integrationen igen."))
            }
            ApiError::Premium => Some((
                "premium",
                "myUplink: kræver Premium",
                "myUplink afviste en ændring af en indstilling. At ændre indstillinger fra andre programmer kræver abonnementet myUplink Premium (Manage) på anlægget.",
            )),
            ApiError::RateLimited(after) => {
                let pause = after.unwrap_or(RATE_LIMIT_PAUSE).max(Duration::from_secs(60));
                warn!("myUplink svarede 429; venter {} minutter", pause.as_secs() / 60);
                *self.paused_until.lock().unwrap() = Some(Instant::now() + pause);
                None
            }
            ApiError::Refused(m) | ApiError::Other(m) => {
                warn!("{m}");
                None
            }
        };
        if let Some((id, title, text)) = issue {
            if let Err(e) = self.host.create_issue(&self.entry_id, id, "error", title, text).await {
                warn!("kunne ikke oprette problem: {e}");
            }
        }
    }

    async fn call_service(&self, entity_id: &str, service: &str, data: &Map<String, Value>) -> Result<(), Error> {
        let (id, kind) = self
            .entities
            .lock()
            .unwrap()
            .iter()
            .find(|(e, _, _)| e == entity_id)
            .map(|(_, d, k)| (d.clone(), k.clone()))
            .ok_or_else(|| Error::new("not_found", entity_id))?;
        let Kind::Point(pid) = kind else { return Err(Error::not_supported(service)) };
        let point = self.devices.lock().unwrap().get(&id).and_then(|d| d.points.get(&pid).cloned()).ok_or_else(|| Error::unavailable(entity_id))?;
        let value = model::write_value(&point, service, data).map_err(|m| if m.is_empty() { Error::not_supported(service) } else { Error::invalid(m) })?;
        if self.paused() {
            return Err(Error::unavailable("myUplink har bedt om en pause"));
        }
        let mut body = Map::new();
        body.insert(pid.clone(), value.clone());
        let r = self.client.set_points(&id, &body).await;
        self.persist().await;
        if let Err(e) = r {
            self.report(&e).await;
            return Err(match e {
                ApiError::Refused(m) => Error::invalid(m),
                e => Error::failed(e),
            });
        }
        let _ = self.host.delete_issue(&self.entry_id, "premium").await;
        if let Some(d) = self.devices.lock().unwrap().get_mut(&id) {
            d.values.insert(pid, value);
        }
        self.publish(Some(&id)).await;
        Ok(())
    }
}
