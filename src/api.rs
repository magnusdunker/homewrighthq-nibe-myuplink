//! OAuth 2.0 and the myUplink API (api.myuplink.com).
//!
//! The user registers an application at dev.myuplink.com. With the client
//! credentials grant the application reaches the systems of the account that
//! registered it, and no browser login is needed. The authorization code
//! grant is there for accounts where that does not work; its refresh token
//! is saved when myUplink replaces it.

use std::time::{Duration, Instant};

use rand::distributions::Alphanumeric;
use rand::Rng;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

pub const AUTHORIZE_URL: &str = "https://api.myuplink.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.myuplink.com/oauth/token";
pub const API_URL: &str = "https://api.myuplink.com";
pub const SCOPE: &str = "READSYSTEM WRITESYSTEM";
pub const SCOPE_OFFLINE: &str = "READSYSTEM WRITESYSTEM offline_access";

#[derive(Clone, Debug, PartialEq)]
pub struct Urls {
    pub authorize: String,
    pub token: String,
    pub api: String,
}

impl Default for Urls {
    fn default() -> Self {
        Self { authorize: AUTHORIZE_URL.into(), token: TOKEN_URL.into(), api: API_URL.into() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// The client or the refresh token was refused.
    Auth(String),
    /// HTTP 403 on a write: changing settings needs myUplink Premium.
    Premium,
    RateLimited(Option<Duration>),
    /// myUplink refused a value (HTTP 400 or 422).
    Refused(String),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth(e) => write!(f, "myUplink afviste login: {e}"),
            ApiError::Premium => write!(f, "at ændre indstillinger kræver myUplink Premium (HTTP 403)"),
            ApiError::RateLimited(_) => write!(f, "myUplink beder om færre forespørgsler (HTTP 429)"),
            ApiError::Refused(e) => write!(f, "myUplink afviste værdien: {e}"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grant {
    ClientCredentials,
    AuthorizationCode,
}

impl Grant {
    pub fn parse(s: &str) -> Self {
        if s == "authorization_code" { Grant::AuthorizationCode } else { Grant::ClientCredentials }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default()
}

pub fn new_state() -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect()
}

pub fn authorize_url(urls: &Urls, client_id: &str, redirect_uri: &str, state: &str) -> String {
    let mut u = reqwest::Url::parse(&urls.authorize).expect("authorize url");
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPE_OFFLINE)
        .append_pair("state", state);
    u.to_string()
}

/// The code from what the user pasted: the whole redirect address, or only
/// the code. `Err` is a form error code.
pub fn parse_redirect(input: &str, state: &str) -> Result<String, &'static str> {
    let s = input.trim();
    if s.is_empty() {
        return Err("missing_code");
    }
    let Ok(u) = reqwest::Url::parse(s) else {
        return if s.contains(char::is_whitespace) { Err("missing_code") } else { Ok(s.to_string()) };
    };
    let get = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.to_string());
    if get("error").is_some() {
        return Err("access_denied");
    }
    if get("state").is_some_and(|st| st != state) {
        return Err("state_mismatch");
    }
    get("code").filter(|c| !c.is_empty()).ok_or("missing_code")
}

async fn token_request(http: &reqwest::Client, urls: &Urls, form: &[(&str, &str)]) -> Result<Tokens, ApiError> {
    let res = http.post(&urls.token).form(form).send().await?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if status.is_client_error() {
        let e = v["error"].as_str().unwrap_or("").to_string();
        let d = v["error_description"].as_str().unwrap_or("").to_string();
        return Err(ApiError::Auth(format!("HTTP {status} {e} {d}").trim().to_string()));
    }
    if !status.is_success() {
        return Err(ApiError::Other(format!("token: HTTP {status}")));
    }
    let access_token = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| ApiError::Other("intet access_token".into()))?.to_string();
    Ok(Tokens { access_token, refresh_token: v["refresh_token"].as_str().map(String::from), expires_in: v["expires_in"].as_u64().unwrap_or(3600) })
}

pub async fn client_credentials(http: &reqwest::Client, urls: &Urls, client_id: &str, secret: &str) -> Result<Tokens, ApiError> {
    token_request(http, urls, &[("grant_type", "client_credentials"), ("client_id", client_id), ("client_secret", secret), ("scope", SCOPE)]).await
}

pub async fn exchange_code(http: &reqwest::Client, urls: &Urls, client_id: &str, secret: &str, redirect_uri: &str, code: &str) -> Result<Tokens, ApiError> {
    token_request(
        http,
        urls,
        &[("grant_type", "authorization_code"), ("code", code), ("redirect_uri", redirect_uri), ("client_id", client_id), ("client_secret", secret)],
    )
    .await
}

pub async fn refresh(http: &reqwest::Client, urls: &Urls, client_id: &str, secret: &str, refresh_token: &str) -> Result<Tokens, ApiError> {
    token_request(http, urls, &[("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", client_id), ("client_secret", secret)]).await
}

struct TokenState {
    access: Option<(String, Instant)>,
    refresh: String,
}

pub struct Client {
    http: reqwest::Client,
    urls: Urls,
    grant: Grant,
    client_id: String,
    secret: String,
    language: String,
    tokens: Mutex<TokenState>,
    new_refresh: std::sync::Mutex<Option<String>>,
}

impl Client {
    pub fn new(urls: Urls, grant: Grant, client_id: &str, secret: &str, refresh_token: &str, access: Option<Tokens>) -> Self {
        let access = access.map(|t| (t.access_token, Instant::now() + Duration::from_secs(t.expires_in)));
        Self {
            http: http(),
            urls,
            grant,
            client_id: client_id.into(),
            secret: secret.into(),
            language: "da".into(),
            tokens: Mutex::new(TokenState { access, refresh: refresh_token.into() }),
            new_refresh: Default::default(),
        }
    }

    /// The language of parameter names, sent as `Accept-Language`.
    pub fn with_language(mut self, language: &str) -> Self {
        if !language.is_empty() {
            self.language = language.into();
        }
        self
    }

    async fn token(&self, force: bool) -> Result<String, ApiError> {
        let mut t = self.tokens.lock().await;
        if !force {
            if let Some((a, until)) = &t.access {
                if Instant::now() + Duration::from_secs(60) < *until {
                    return Ok(a.clone());
                }
            }
        }
        let new = match self.grant {
            Grant::ClientCredentials => client_credentials(&self.http, &self.urls, &self.client_id, &self.secret).await?,
            Grant::AuthorizationCode => {
                let n = refresh(&self.http, &self.urls, &self.client_id, &self.secret, &t.refresh).await?;
                if let Some(r) = n.refresh_token.clone().filter(|r| *r != t.refresh) {
                    t.refresh = r.clone();
                    *self.new_refresh.lock().unwrap() = Some(r);
                }
                n
            }
        };
        t.access = Some((new.access_token.clone(), Instant::now() + Duration::from_secs(new.expires_in)));
        Ok(new.access_token)
    }

    pub fn take_new_refresh(&self) -> Option<String> {
        self.new_refresh.lock().unwrap().take()
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Option<Value>, ApiError> {
        for attempt in 0..2 {
            let token = self.token(attempt > 0).await?;
            let mut req = self.http.request(method.clone(), format!("{}{path}", self.urls.api)).bearer_auth(token).header("Accept-Language", &self.language);
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await?;
            let status = res.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                continue;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).map(Duration::from_secs);
                return Err(ApiError::RateLimited(after));
            }
            let text = res.text().await.unwrap_or_default();
            let short = || text.chars().take(200).collect::<String>();
            match status.as_u16() {
                401 => return Err(ApiError::Auth("HTTP 401".into())),
                403 if method != reqwest::Method::GET => return Err(ApiError::Premium),
                400 | 422 => return Err(ApiError::Refused(format!("HTTP {status}: {}", short()))),
                _ if !status.is_success() => return Err(ApiError::Other(format!("HTTP {status}: {}", short()))),
                _ => {}
            }
            if text.trim().is_empty() {
                return Ok(None);
            }
            return serde_json::from_str(&text).map(Some).map_err(|e| ApiError::Other(format!("ugyldigt svar: {e}")));
        }
        Err(ApiError::Auth("HTTP 401".into()))
    }

    /// The account's systems with their devices.
    pub async fn systems(&self) -> Result<Value, ApiError> {
        Ok(self.send(reqwest::Method::GET, "/v2/systems/me?page=1&itemsPerPage=100", None).await?.unwrap_or(json!({})))
    }

    /// Every data point of a device.
    pub async fn points(&self, device_id: &str) -> Result<Value, ApiError> {
        Ok(self.send(reqwest::Method::GET, &format!("/v2/devices/{device_id}/points"), None).await?.unwrap_or(json!([])))
    }

    /// Write data points, `{ "47011": 1 }`. Needs myUplink Premium.
    pub async fn set_points(&self, device_id: &str, values: &Map<String, Value>) -> Result<Option<Value>, ApiError> {
        self.send(reqwest::Method::PATCH, &format!("/v2/devices/{device_id}/points"), Some(&Value::Object(values.clone()))).await
    }
}
