//! Small helpers: time, random ids, hashing.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub fn now_s() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// ISO UTC with milliseconds, like JavaScript's `toISOString()`.
pub fn iso_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn iso_from_s(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default()
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the operating system's random generator failed");
    out
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `usr_…` / `dev_…` ids: prefix + 22 base64url characters (128 random bits).
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", b64(&random_bytes::<16>()))
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

/// Pairing codes: 8 characters without ambiguous ones (0/O, 1/I/L).
pub const PAIRING_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

pub fn new_pairing_code() -> String {
    let mut out = String::with_capacity(8);
    let mut buf = random_bytes::<64>().into_iter();
    while out.len() < 8 {
        let b = buf.next().unwrap_or_else(|| random_bytes::<1>()[0]);
        // Rejection sampling: 31 symbols, keep bytes below 248 (8 × 31) to avoid bias.
        if b < 248 {
            out.push(PAIRING_ALPHABET[(b % 31) as usize] as char);
        }
    }
    out
}

/// Uppercase and drop spaces/dashes, as clients may type `abcd-2345`.
pub fn normalize_pairing_code(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_uppercase()
}

/// `ABCD-2345` for display.
pub fn format_pairing_code(code: &str) -> String {
    if code.len() == 8 { format!("{}-{}", &code[..4], &code[4..]) } else { code.to_string() }
}

/// Minimal URL query encoding (RFC 3986 unreserved characters kept).
pub fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
