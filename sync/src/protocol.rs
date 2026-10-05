//! JSON wire types of the `/v1` API (SPEC.md section 7). Field names are camelCase.

use serde::{Deserialize, Serialize};

use crate::crypto::KdfParams;
use crate::stats::HistoryStats;

pub const API_PREFIX: &str = "/v1";
pub const TOKEN_PREFIX: &str = "wcs_";
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_PUSH_RECORDS: usize = 500;
pub const TARGET_PUSH_BYTES: usize = 768 * 1024;
pub const MAX_PULL_BYTES: usize = 1024 * 1024;

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
    #[serde(default)]
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub sync_report: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReportRequest {
    pub uploaded: u64,
    pub downloaded: u64,
    #[serde(default)]
    pub pending: u64,
    pub app_version: String,
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

/// A prefix whose actual UTF-8 JSON size fits comfortably below the request limit.
/// Individual oversized records must be handled by the caller before using this helper.
pub fn push_batch_len(changes: &[Change]) -> usize {
    let mut bytes = b"{\"changes\":[]}".len();
    let mut count = 0;
    for change in changes.iter().take(MAX_PUSH_RECORDS) {
        let size = serde_json::to_vec(change).expect("serializable change").len() + usize::from(count > 0);
        if count > 0 && bytes + size > TARGET_PUSH_BYTES {
            break;
        }
        bytes += size;
        count += 1;
    }
    count
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
    /// Non-failed entries missing a price/estimate for either applicable STT or LLM component.
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batches_count_serialized_utf8_and_json_escaping() {
        let changes: Vec<_> = (0..30)
            .map(|id| Change {
                kind: "setting".into(),
                id: id.to_string(),
                hlc: "0".into(),
                payload: Some("\"é\\".repeat(10_000)),
                seq: None,
                deleted: false,
                device: None,
                stats: None,
            })
            .collect();
        let mut remaining = changes.as_slice();
        let mut batches = 0;
        while !remaining.is_empty() {
            let count = push_batch_len(remaining);
            assert!(count > 0 && count < 30);
            let body = serde_json::to_vec(&PushRequest { changes: remaining[..count].to_vec() }).unwrap();
            assert!(body.len() <= TARGET_PUSH_BYTES && body.len() < MAX_REQUEST_BYTES);
            remaining = &remaining[count..];
            batches += 1;
        }
        assert!(batches > 1);
    }
}
