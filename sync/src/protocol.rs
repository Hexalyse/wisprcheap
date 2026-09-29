//! JSON wire types of the `/v1` API (SPEC.md section 7). Field names are camelCase.

use serde::{Deserialize, Serialize};

use crate::crypto::KdfParams;
use crate::stats::HistoryStats;

pub const API_PREFIX: &str = "/v1";
pub const TOKEN_PREFIX: &str = "wcs_";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub error: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub code: String,
    pub name: String,
    /// `windows`, `linux`, `macos` or `android`.
    pub platform: String,
    pub app_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResponse {
    pub token: String,
    pub device: DeviceInfo,
    pub user: UserInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyring {
    pub key_version: i64,
    /// Fingerprint of the data key (`DataKey::key_id`), to notice an encryption reset.
    pub key_id: String,
    /// base64url, 16 bytes.
    pub salt: String,
    pub kdf: KdfParams,
    /// `e1.…` envelope of the data key under the passphrase key.
    pub wrapped_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub user: UserInfo,
    pub device: DeviceInfo,
    pub server_time: String,
    pub server_version: String,
    pub keyring: Option<Keyring>,
}

/// Body of `PUT /v1/keyring` (create; or replace with `If-Match: <keyVersion>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutKeyringRequest {
    pub key_id: String,
    pub salt: String,
    pub kdf: KdfParams,
    pub wrapped_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutKeyringResponse {
    pub key_version: i64,
}

/// One record in the change feed, or one change pushed by a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Server sequence number (set by the server in the feed; ignored when pushing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    pub kind: String,
    pub id: String,
    pub hlc: String,
    #[serde(default)]
    pub deleted: bool,
    /// `e1.…` envelope; `null` for deletions.
    #[serde(default)]
    pub payload: Option<String>,
    /// Device that made the change (set by the server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Readable statistics, for `kind = "history"` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<HistoryStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResponse {
    pub changes: Vec<Change>,
    pub next_since: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushRequest {
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushStatus {
    /// Stored (the record is now `seq`).
    Applied,
    /// A newer version exists on the server: see `current`.
    Stale,
    /// History entry already stored: nothing to do.
    Exists,
    /// Invalid change: see `error`.
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub kind: String,
    pub id: String,
    pub status: PushStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResponse {
    pub results: Vec<PushResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameDeviceRequest {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMonth {
    pub device: String,
    pub entries: u64,
    pub words: u64,
    pub total_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthStats {
    /// `YYYY-MM` in the requested time zone offset.
    pub month: String,
    /// Successful entries (no error, some words), dictations and commands.
    pub entries: u64,
    pub commands: u64,
    pub failed: u64,
    pub words: u64,
    pub audio_minutes: f64,
    pub stt_usd: f64,
    pub llm_usd: f64,
    pub total_usd: f64,
    /// Entries whose model has no known price (counted as $0).
    pub unknown_price: u64,
    pub by_device: Vec<DeviceMonth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsResponse {
    /// Newest first.
    pub months: Vec<MonthStats>,
    pub devices: Vec<DeviceInfo>,
}
