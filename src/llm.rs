//! HTTP plumbing and the OpenAI-compatible Chat Completions call.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::config::LlmOptions;

/// A request that failed before an HTTP response arrived (network error or timeout): worth one retry.
#[derive(Debug)]
pub struct NetworkError(pub String);

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NetworkError {}

/// Shared client, so connections are kept alive between dictations.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("wisprcheap/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client")
    })
}

/// Turn a reqwest error into a `NetworkError` (with its causes) or a plain error.
pub fn request_error(e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        return NetworkError("The operation was aborted due to timeout".into()).into();
    }
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        let text = s.to_string();
        if !msg.contains(&text) {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        source = s.source();
    }
    if e.is_decode() {
        anyhow!(msg)
    } else {
        NetworkError(msg).into()
    }
}

pub fn is_network_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NetworkError>().is_some()
}

/// "HTTP 401 Unauthorized: <body>" for a non-2xx response.
pub async fn read_error(res: reqwest::Response) -> String {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let reason = status.canonical_reason().unwrap_or("");
    let mut msg = format!("HTTP {} {}", status.as_u16(), reason);
    if !body.is_empty() {
        msg.push_str(": ");
        msg.extend(body.chars().take(500));
    }
    msg
}

pub fn trim_slash(url: &str) -> &str {
    url.strip_suffix('/').unwrap_or(url)
}

#[derive(Debug, Clone, Default)]
pub struct ChatResult {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One non-streaming call to an OpenAI-compatible Chat Completions endpoint.
pub async fn chat_complete(
    opts: &LlmOptions,
    system: &str,
    user: &str,
    label: &str,
) -> Result<ChatResult> {
    let mut body = json!({
        "model": opts.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
    });
    if let Some(effort) = opts.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
        body["reasoning_effort"] = json!(effort);
    }
    if let Some(t) = opts.temperature {
        body["temperature"] = json!(t);
    }

    let mut req = client()
        .post(format!("{}/chat/completions", trim_slash(&opts.base_url)))
        .timeout(Duration::from_millis(opts.timeout_ms))
        .json(&body);
    if let Some(key) = opts.api_key.as_deref().filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }
    let res = req.send().await.map_err(request_error)?;
    if !res.status().is_success() {
        return Err(anyhow!("{label}: {}", read_error(res).await));
    }
    let json: Value = res.json().await.map_err(request_error)?;
    Ok(ChatResult {
        text: json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        input_tokens: json["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: json["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    })
}
