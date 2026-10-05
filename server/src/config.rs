//! Server configuration from environment variables (`WCS_*`).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct Config {
    /// Public base URL (no trailing slash), used in QR codes and to decide cookie security.
    pub public_url: String,
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    /// Trust `X-Forwarded-For` (only behind a reverse proxy).
    pub trust_proxy: bool,
    /// Allow a plain-HTTP public URL other than localhost (LAN without TLS).
    pub allow_http: bool,
    /// Zero keeps history/audit records indefinitely.
    pub history_retention_days: u32,
    pub audit_retention_days: u32,
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"))
}

fn env_days(name: &str) -> Result<u32> {
    let text = std::env::var(name).unwrap_or_else(|_| "0".into());
    let days =
        text.parse::<u32>().with_context(|| format!("{name}: expected a number of days (0 keeps everything)"))?;
    if days > 36_500 {
        bail!("{name}: must be at most 36500");
    }
    Ok(days)
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let public_url = std::env::var("WCS_PUBLIC_URL").unwrap_or_else(|_| "http://localhost:8080".into());
        let bind = std::env::var("WCS_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let config = Self {
            public_url: public_url.trim().trim_end_matches('/').to_string(),
            bind: bind.parse().with_context(|| format!("WCS_BIND: invalid address {bind:?}"))?,
            data_dir: std::env::var("WCS_DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("data")),
            trust_proxy: env_flag("WCS_TRUST_PROXY"),
            allow_http: env_flag("WCS_ALLOW_HTTP"),
            history_retention_days: env_days("WCS_HISTORY_RETENTION_DAYS")?,
            audit_retention_days: env_days("WCS_AUDIT_RETENTION_DAYS")?,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let url = &self.public_url;
        if !url.starts_with("https://") && !url.starts_with("http://") {
            bail!("WCS_PUBLIC_URL must start with https:// (got {url:?})");
        }
        if !self.is_https() && !self.is_localhost() && !self.allow_http {
            bail!(
                "WCS_PUBLIC_URL is plain HTTP ({url}). Put the server behind an HTTPS reverse proxy and use its https:// URL, \
                 or set WCS_ALLOW_HTTP=1 for a trusted LAN."
            );
        }
        Ok(())
    }

    pub fn is_https(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    fn is_localhost(&self) -> bool {
        let rest = self.public_url.split("://").nth(1).unwrap_or("");
        ["localhost", "127.0.0.1", "[::1]"]
            .iter()
            .any(|h| rest == *h || rest.starts_with(&format!("{h}:")) || rest.starts_with(&format!("{h}/")))
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("wisprcheap.db")
    }

    /// For tests: a local server with its database in `dir`.
    pub fn for_tests(dir: &std::path::Path) -> Self {
        Self {
            public_url: "http://localhost:8080".into(),
            bind: "127.0.0.1:0".parse().expect("address"),
            data_dir: dir.to_path_buf(),
            trust_proxy: false,
            allow_http: false,
            history_retention_days: 0,
            audit_retention_days: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_plain_http_on_the_internet() {
        let mut c = Config::for_tests(std::path::Path::new("."));
        assert!(c.validate().is_ok());
        c.public_url = "http://sync.example.com".into();
        assert!(c.validate().is_err());
        c.allow_http = true;
        assert!(c.validate().is_ok());
        c.public_url = "https://sync.example.com".into();
        c.allow_http = false;
        assert!(c.validate().is_ok() && c.is_https());
    }
}
