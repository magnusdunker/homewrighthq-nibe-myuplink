//! Mapping tests on answers shaped like myUplink's, and the plugin against a
//! fake myUplink with HomewrightHQ played over a pipe.

mod host;

use homewright_nibe_myuplink::api;
use homewright_nibe_myuplink::model::{self, Kind};
use homewright_nibe_myuplink::MyUplink;
use serde_json::{json, Map, Value};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEV: &str = "emmy-r-12345-20201231-06589200000000-ab-cd-ef-00-11-22";

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn point(id: &str) -> (model::Point, Value) {
    let all = fixture("points.json");
    let p = all.as_array().unwrap().iter().find(|p| p["parameterId"] == id).unwrap().clone();
    (model::parse_point(&p).unwrap(), p["value"].clone())
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

#[test]
fn devices_from_systems() {
    let d = model::devices(&fixture("systems_me.json"));
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].id, DEV);
    // one device in the system: it takes the system's name
    assert_eq!(d[0].name, "Hjem");
    assert_eq!(d[0].manufacturer.as_deref(), Some("NIBE"));
    assert_eq!(d[0].model.as_deref(), Some("S1255"));
    assert_eq!(d[0].serial.as_deref(), Some("06589200000000"));
    assert!(d[0].connected);
}

#[test]
fn points_become_sensors_numbers_and_selects() {
    let (p, v) = point("40004");
    assert_eq!(p.name, "Current outdoor temp (BT1)");
    assert_eq!(p.kind, Kind::Number);
    assert_eq!(model::domain(&p), "sensor");
    assert_eq!(model::state(&p, &v).0, "-1.3");
    assert_eq!(model::classes(&p.unit), (Some("temperature"), Some("measurement"), Some("°C".into())));
    assert_eq!(model::classes("kWh").1, Some("total_increasing"));

    let (p, v) = point("49994");
    assert_eq!(model::domain(&p), "sensor");
    assert_eq!(model::state(&p, &v).0, "Heating");

    // min, max and step are scaled; the value is not
    let (p, v) = point("47398");
    assert_eq!(p.kind, Kind::Setting { min: 5.0, max: 35.0, step: 0.5 });
    assert_eq!(model::domain(&p), "number");
    let (s, a) = model::state(&p, &v);
    assert_eq!(s, "21.5");
    assert_eq!(a["min"], 5.0);
    assert_eq!(model::write_value(&p, "set_value", &obj(json!({ "value": 22.3 }))).unwrap(), json!(22.5));
    assert!(model::write_value(&p, "set_value", &obj(json!({ "value": 40 }))).is_err());

    let (p, v) = point("47011");
    assert_eq!(p.kind, Kind::Setting { min: -10.0, max: 10.0, step: 1.0 });
    assert_eq!(model::write_value(&p, "set_value", &obj(json!({ "value": "-2" }))).unwrap(), json!(-2.0));

    let (p, v2) = point("47041");
    assert_eq!(model::domain(&p), "select");
    let (s, a) = model::state(&p, &v2);
    assert_eq!(s, "Normal");
    assert_eq!(a["options"], json!(["Small", "Normal", "Large"]));
    assert_eq!(model::write_value(&p, "select_option", &obj(json!({ "option": "Large" }))).unwrap(), json!(2));
    assert!(model::write_value(&p, "select_option", &obj(json!({ "option": "Huge" }))).is_err());
    assert_eq!(model::write_value(&p, "set_value", &Map::new()).unwrap_err(), "");
    let _ = v;
}

#[test]
fn duplicate_option_texts_stay_apart() {
    let o = vec![("0".to_string(), "Off".to_string()), ("1".to_string(), "Off".to_string())];
    assert_eq!(model::option_texts(&o), vec!["Off", "Off (1)"]);
}

#[test]
fn authorize_address() {
    let url = api::authorize_url(&api::Urls::default(), "cid", "http://localhost:8123/auth/external/callback", "s1");
    assert!(url.starts_with("https://api.myuplink.com/oauth/authorize?response_type=code&client_id=cid"));
    assert!(url.contains("scope=READSYSTEM+WRITESYSTEM+offline_access&state=s1"));
    assert_eq!(api::parse_redirect("http://localhost:8123/auth/external/callback?code=c1&state=s1", "s1"), Ok("c1".into()));
    assert_eq!(api::parse_redirect("http://x/?code=c1&state=s2", "s1"), Err("state_mismatch"));
}

async fn cloud(server: &MockServer, premium: bool) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_secret=sec"))
        .and(body_string_contains("scope=READSYSTEM+WRITESYSTEM"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "T1", "expires_in": 3600, "token_type": "Bearer", "scope": "READSYSTEM WRITESYSTEM" })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_client" })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/systems/me"))
        .and(header("authorization", "Bearer T1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("systems_me.json")))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v2/devices/{DEV}/points")))
        .and(header("authorization", "Bearer T1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("points.json")))
        .mount(server)
        .await;
    let answer = if premium { ResponseTemplate::new(200).set_body_json(json!([{ "status": "modified" }])) } else { ResponseTemplate::new(403) };
    Mock::given(method("PATCH")).and(path(format!("/v2/devices/{DEV}/points"))).respond_with(answer).mount(server).await;
}

fn flow(data: Value) -> Value {
    json!({ "flow_id": "f1", "source": "user", "data": data })
}

fn urls(server: &MockServer) -> Value {
    json!({ "authorize_url": format!("{}/oauth/authorize", server.uri()), "token_url": format!("{}/oauth/token", server.uri()), "api_url": server.uri() })
}

#[tokio::test(flavor = "multi_thread")]
async fn client_credentials_setup_and_writes() {
    let server = MockServer::start().await;
    cloud(&server, true).await;
    let h = host::FakeHost::start(MyUplink::default());
    let u = urls(&server);

    let r = h.call("config_flow", json!({ "flow": flow(u.clone()), "step": "user", "input": null })).await.unwrap();
    assert_eq!(r["result"]["schema"]["properties"]["grant"]["default"], "client_credentials");

    let bad = json!({ "client_id": "cid", "client_secret": "wrong", "grant": "client_credentials" });
    let r = h.call("config_flow", json!({ "flow": flow(u.clone()), "step": "user", "input": bad })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_client");

    let ok = json!({ "client_id": "cid", "client_secret": "sec", "grant": "client_credentials", "redirect_uri": "" });
    let r = h.call("config_flow", json!({ "flow": flow(u.clone()), "step": "user", "input": ok })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "myUplink (Hjem)");
    assert_eq!(r["unique_id"], "cid");
    let data = r["result"]["data"].clone();
    assert!(data.get("refresh_token").is_none());

    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "myuplink", "data": data } })).await.unwrap();
    assert_eq!(h.rec.lock().unwrap().entities.len(), 9);
    assert_eq!(h.state(&h.entity(&format!("{DEV}-40004"))).unwrap().0, "-1.3");
    assert_eq!(h.state(&h.entity(&format!("{DEV}-49994"))).unwrap().0, "Heating");
    assert_eq!(h.state(&h.entity(&format!("{DEV}-alarm"))).unwrap().0, "off");
    let setpoint = h.entity(&format!("{DEV}-47398"));
    assert!(setpoint.starts_with("number."));
    let (spec, handler) = h.rec.lock().unwrap().entities[&setpoint].clone();
    assert!(handler);
    assert_eq!(spec["entity_category"], "config");
    assert_eq!(spec["capabilities"]["max"], 35.0);
    let energy = h.entity(&format!("{DEV}-44300"));
    let (spec, handler) = h.rec.lock().unwrap().entities[&energy].clone();
    assert!(!handler);
    assert_eq!(spec["device_class"], "energy");

    let call = |entity: String, service: &str, data: Value| json!({ "entry_id": "e1", "entity_id": entity, "service": service, "data": data });
    h.call("entity/call_service", call(setpoint.clone(), "set_value", json!({ "value": 22 }))).await.unwrap();
    assert_eq!(h.state(&setpoint).unwrap().0, "22");
    let mode = h.entity(&format!("{DEV}-47041"));
    h.call("entity/call_service", call(mode.clone(), "select_option", json!({ "option": "Large" }))).await.unwrap();
    assert_eq!(h.state(&mode).unwrap().0, "Large");

    let sent: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(sent, vec![json!({ "47398": 22.0 }), json!({ "47041": 2 })]);
    // the refused secret, the flow and the setup each ask for a token
    let tokens = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/oauth/token").count();
    assert_eq!(tokens, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_without_premium_make_an_issue() {
    let server = MockServer::start().await;
    cloud(&server, false).await;
    let h = host::FakeHost::start(MyUplink::default());
    let mut data = urls(&server);
    data["client_id"] = json!("cid");
    data["client_secret"] = json!("sec");
    data["grant"] = json!("client_credentials");
    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "myuplink", "data": data } })).await.unwrap();
    let setpoint = h.entity(&format!("{DEV}-47011"));
    let err = h
        .call("entity/call_service", json!({ "entry_id": "e1", "entity_id": setpoint, "service": "set_value", "data": { "value": 1 } }))
        .await
        .unwrap_err();
    assert_eq!(err["data"]["kind"], "failed");
    assert!(h.rec.lock().unwrap().issues.contains(&"premium".to_string()));
    assert_eq!(h.state(&setpoint).unwrap().0, "0");
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_login_saves_the_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "T1", "refresh_token": "R1", "expires_in": 3600 })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=R1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "T1", "refresh_token": "R2", "expires_in": 3600 })))
        .mount(&server)
        .await;
    cloud(&server, true).await;
    let h = host::FakeHost::start(MyUplink::default());
    let input = json!({ "client_id": "cid", "client_secret": "sec", "grant": "authorization_code", "redirect_uri": "https://hjem.example/auth/external/callback" });
    let r = h.call("config_flow", json!({ "flow": flow(urls(&server)), "step": "user", "input": input })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "auth");
    let data = r["data"].clone();
    let state = data["oauth_state"].as_str().unwrap().to_string();
    let pasted = json!({ "redirect_url": format!("https://hjem.example/auth/external/callback?code=c1&state={state}") });
    let r = h.call("config_flow", json!({ "flow": flow(data), "step": "auth", "input": pasted })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    let entry = r["result"]["data"].clone();
    assert_eq!(entry["refresh_token"], "R1");
    assert_eq!(entry["grant"], "authorization_code");

    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "myuplink", "data": entry } })).await.unwrap();
    assert_eq!(h.rec.lock().unwrap().entry_updates.last().unwrap()["refresh_token"], "R2");
}
