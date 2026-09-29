//! HTTP client of the sync API (`/v1`, see `sync/SPEC.md` section 7).

use std::fmt;
use std::time::Duration;

use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use wisprcheap_sync::protocol::{
    Change, ChangesResponse, ErrorBody, MeResponse, PairRequest, PairResponse, PushRequest,
    PushResponse, PutKeyringRequest, PutKeyringResponse, RenameDeviceRequest, StatsResponse,
};

#[derive(Debug, Clone)]
pub enum ApiError {
    /// The token was revoked (or the account disabled).
    Unauthorized,
    /// The server couldn't be reached.
    Network(String),
    /// The server answered with an error.
    Status {
        status: u16,
        code: String,
        message: String,
    },
}

impl ApiError {
    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Status { code, .. } => Some(code),
            _ => None,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "this device was disconnected from the server"),
            ApiError::Network(e) => write!(f, "can't reach the server: {e}"),
            ApiError::Status {
                status,
                code,
                message,
            } => {
                if message.is_empty() {
                    write!(f, "server error {status} ({code})")
                } else {
                    write!(f, "{message} ({code}, HTTP {status})")
                }
            }
        }
    }
}

impl std::error::Error for ApiError {}

/// `https://sync.example.com` from what the user typed (scheme added, trailing `/` removed).
pub fn normalize_server(url: &str) -> Result<String, String> {
    let t = url.trim().trim_end_matches('/');
    if t.is_empty() {
        return Err("the server URL is empty".into());
    }
    let full = if t.starts_with("https://") || t.starts_with("http://") {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    if full.contains(char::is_whitespace) {
        return Err(format!("invalid server URL: {url}"));
    }
    Ok(full)
}

fn describe(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        let s_text = s.to_string();
        if !text.contains(&s_text) {
            text.push_str(": ");
            text.push_str(&s_text);
        }
        source = s.source();
    }
    text
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    pub fn new(server: &str, token: &str) -> Self {
        Self {
            http: http_client(),
            base: server.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    pub async fn me(&self) -> Result<MeResponse, ApiError> {
        send(self.request(Method::GET, "/v1/me")).await
    }

    pub async fn put_keyring(
        &self,
        req: &PutKeyringRequest,
        if_match: Option<i64>,
    ) -> Result<PutKeyringResponse, ApiError> {
        let mut r = self.request(Method::PUT, "/v1/keyring").json(req);
        if let Some(v) = if_match {
            r = r.header(reqwest::header::IF_MATCH, v.to_string());
        }
        send(r).await
    }

    pub async fn pull(
        &self,
        since: i64,
        limit: usize,
        exclude_history: bool,
    ) -> Result<ChangesResponse, ApiError> {
        let mut path = format!("/v1/changes?since={since}&limit={limit}");
        if exclude_history {
            path.push_str("&exclude=history");
        }
        send(self.request(Method::GET, &path)).await
    }

    pub async fn push(&self, changes: &[Change]) -> Result<PushResponse, ApiError> {
        let body = PushRequest {
            changes: changes.to_vec(),
        };
        send(self.request(Method::POST, "/v1/changes").json(&body)).await
    }

    pub async fn stats(&self, from: &str, to: &str, offset: i64) -> Result<StatsResponse, ApiError> {
        let path = format!("/v1/stats?from={from}&to={to}&offset={offset}");
        send(self.request(Method::GET, &path)).await
    }

    pub async fn rename(&self, name: &str) -> Result<(), ApiError> {
        let body = RenameDeviceRequest { name: name.into() };
        send_empty(self.request(Method::PATCH, "/v1/device").json(&body)).await
    }

    pub async fn unpair(&self) -> Result<(), ApiError> {
        send_empty(self.request(Method::DELETE, "/v1/device")).await
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(format!("wisprcheap/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// `POST /v1/pair` (no token yet).
pub async fn pair(server: &str, req: &PairRequest) -> Result<PairResponse, ApiError> {
    let url = format!("{}/v1/pair", server.trim_end_matches('/'));
    send(http_client().post(url).json(req)).await
}

async fn response(r: RequestBuilder) -> Result<reqwest::Response, ApiError> {
    let res = r.send().await.map_err(|e| ApiError::Network(describe(&e)))?;
    let status = res.status();
    if status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::Unauthorized);
    }
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        let body: Option<ErrorBody> = serde_json::from_str(&text).ok();
        return Err(ApiError::Status {
            status: status.as_u16(),
            code: body.as_ref().map(|b| b.error.clone()).unwrap_or_else(|| "error".into()),
            message: body.map(|b| b.message).unwrap_or_else(|| {
                let t: String = text.chars().take(200).collect();
                t.trim().to_string()
            }),
        });
    }
    Ok(res)
}

async fn send<T: DeserializeOwned>(r: RequestBuilder) -> Result<T, ApiError> {
    let res = response(r).await?;
    let status = res.status().as_u16();
    let bytes = res
        .bytes()
        .await
        .map_err(|e| ApiError::Network(describe(&e)))?;
    serde_json::from_slice(&bytes).map_err(|e| ApiError::Status {
        status,
        code: "bad_response".into(),
        message: format!("unexpected answer from the server ({e}); is this a wisprcheap sync server?"),
    })
}

async fn send_empty(r: RequestBuilder) -> Result<(), ApiError> {
    response(r).await.map(|_| ())
}
