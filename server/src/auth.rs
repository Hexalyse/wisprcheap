//! Passwords, rate limits, client addresses and the device-token extractor.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::SharedState;
use crate::db::{self, Device, User};
use crate::error::ApiError;

pub const MIN_PASSWORD_LEN: usize = 12;

fn argon2() -> Argon2<'static> {
    // OWASP minimum for Argon2id: 19 MiB, 2 iterations, 1 lane.
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(19_456, 2, 1, None).expect("valid Argon2 parameters"))
}

/// PHC string (`$argon2id$v=19$m=…`), so parameters can change later.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = crate::util::random_bytes::<16>();
    Ok(argon2()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("password hash: {e}"))?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordVerifier::<str>::verify_password(&argon2(), password.as_bytes(), hash).is_ok()
}

/// Hash compared against when the user doesn't exist, so a failed login takes the same time either way.
pub fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| hash_password("not a real password, only for timing").expect("hash"))
}

pub fn check_password_rules(username: &str, password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!("The password must have at least {MIN_PASSWORD_LEN} characters."));
    }
    if password.eq_ignore_ascii_case(username) {
        return Err("The password must differ from the username.".into());
    }
    Ok(())
}

pub fn check_username(username: &str) -> Result<(), String> {
    let ok = (2..=40).contains(&username.len())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if ok { Ok(()) } else { Err("Usernames have 2 to 40 characters: letters, digits, dot, dash or underscore.".into()) }
}

// --- Rate limiting (in memory) ---

#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<String, (Instant, u32)>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    /// Fixed-window limit: at most `max` calls per `window` for `key`.
    pub fn allow(&self, key: &str, max: u32, window: Duration) -> bool {
        let mut map = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        if map.len() > 10_000 {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= max
    }

    /// Login throttling: after 5 failures, wait 30 s, doubling up to 15 min. Returns the wait left.
    pub fn login_blocked(&self, key: &str) -> Option<Duration> {
        let map = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let (count, last) = map.get(key)?;
        if *count < 5 {
            return None;
        }
        let wait = Duration::from_secs((30u64 << (count - 5).min(5)).min(900));
        let elapsed = last.elapsed();
        (elapsed < wait).then(|| wait - elapsed)
    }

    pub fn login_failed(&self, key: &str) {
        let mut map = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let entry = map.entry(key.to_string()).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
    }

    pub fn login_succeeded(&self, key: &str) {
        self.failures.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
    }
}

/// Client address: the socket peer, or the first `X-Forwarded-For` hop behind a trusted proxy.
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(first) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return first.to_string();
        }
    }
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".into())
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(axum::http::header::COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
        v.split(';').find_map(|pair| {
            let (k, val) = pair.trim().split_once('=')?;
            (k == name).then(|| val.to_string())
        })
    })
}

/// The client's address (see [`client_ip`]); never fails.
pub struct ClientIp(pub String);

impl FromRequestParts<SharedState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        Ok(Self(client_ip(&parts.headers, peer, state.config.trust_proxy)))
    }
}

/// The device making an API request (`Authorization: Bearer wcs_…`).
pub struct DeviceAuth {
    pub device: Device,
    pub user: User,
    pub ip: String,
}

impl FromRequestParts<SharedState> for DeviceAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| t.starts_with(wisprcheap_sync::protocol::TOKEN_PREFIX))
            .ok_or_else(ApiError::unauthorized)?;
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        let ip = client_ip(&parts.headers, peer, state.config.trust_proxy);
        let conn = state.db.lock();
        let (device, user) = db::device_by_token(&conn, token)?.ok_or_else(ApiError::unauthorized)?;
        if !state.limiter.allow(&format!("api:{}", device.id), 120, Duration::from_secs(60)) {
            return Err(ApiError::rate_limited());
        }
        db::touch_device(&conn, &device.id, &ip)?;
        Ok(Self { device, user, ip })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        let h = hash_password("a long passphrase").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("a long passphrase", &h));
        assert!(!verify_password("a long passphrasE", &h));
        assert!(!verify_password("x", "not a hash"));
        assert!(check_password_rules("bob", "short").is_err());
        assert!(check_password_rules("bob", "long enough password").is_ok());
        assert!(check_username("a b").is_err() && check_username("hexa_lyse").is_ok());
    }

    #[test]
    fn limits() {
        let l = RateLimiter::default();
        assert!((0..3).all(|_| l.allow("k", 3, Duration::from_secs(60))));
        assert!(!l.allow("k", 3, Duration::from_secs(60)));
        for _ in 0..4 {
            l.login_failed("u");
        }
        assert!(l.login_blocked("u").is_none());
        l.login_failed("u");
        assert!(l.login_blocked("u").is_some());
        l.login_succeeded("u");
        assert!(l.login_blocked("u").is_none());
    }

    #[test]
    fn forwarded_for_only_when_trusted() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.5, 10.0.0.1".parse().unwrap());
        let peer: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        assert_eq!(client_ip(&h, Some(peer), true), "203.0.113.5");
        assert_eq!(client_ip(&h, Some(peer), false), "10.0.0.1");
    }
}
