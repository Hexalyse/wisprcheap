//! What a device remembers between syncs: `<cache dir>/sync/state.json` (identity, cursor, snapshot
//! of the last synced profile, outbox, remote changes not applied yet, history upload position).
//! A lock file keeps the app and the CLI from syncing at the same time.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use wisprcheap_sync::protocol::Change;

const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = "lock";

fn is_false(b: &bool) -> bool {
    !*b
}

/// Last synced version of a record: its HLC and a keyed hash of the value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snap {
    pub hlc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncState {
    pub server: String,
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub device_name: String,
    /// Key id of the data key the snapshot was made with (a new key means starting over).
    pub key_id: String,
    /// Change feed position.
    pub cursor: i64,
    /// The first sync (merge) is done.
    pub initialized: bool,
    /// Last HLC issued or seen.
    pub hlc: String,
    /// `kind/id` → last synced version.
    pub snapshot: BTreeMap<String, Snap>,
    /// Local changes not pushed yet (encrypted).
    pub outbox: Vec<Change>,
    /// Remote changes that couldn't be written to the config yet (encrypted).
    pub pending: Vec<Change>,
    /// History file and how many bytes of it were uploaded.
    pub history_file: String,
    pub history_offset: u64,
    /// `sync.history` used for the cursor (switching to `download` pulls everything again).
    pub history_mode: String,
    pub last_sync: Option<String>,
}

pub fn record_key(kind: &str, id: &str) -> String {
    format!("{kind}/{id}")
}

pub fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once('/')
}

impl SyncState {
    pub fn load(dir: &Path) -> Self {
        let file = dir.join(STATE_FILE);
        match std::fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                crate::warn!("[sync] {} is damaged ({e}); starting over.", file.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let file = dir.join(STATE_FILE);
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &file).map_err(|e| anyhow!("can't write {}: {e}", file.display()))
    }

    /// Forget everything synced (new data key): the next sync is a first sync again.
    pub fn reset_data(&mut self) {
        self.cursor = 0;
        self.initialized = false;
        self.snapshot.clear();
        self.outbox.clear();
        self.pending.clear();
        self.history_offset = 0;
        self.history_mode.clear();
    }

    /// Queue a local change, replacing an older queued change of the same record.
    pub fn queue(&mut self, change: Change) {
        self.outbox
            .retain(|o| !(o.kind == change.kind && o.id == change.id));
        self.outbox.push(change);
    }
}

/// `<cache dir>/sync` (per instance when WISPRCHEAP_INSTANCE is set).
pub fn default_state_dir() -> PathBuf {
    let name = match crate::paths::instance_suffix() {
        Some(s) => format!("sync-{s}"),
        None => "sync".to_string(),
    };
    crate::paths::paths().cache_dir.join(name)
}

/// The device id stored by pairing (for new history entries).
pub fn device_id(dir: &Path) -> Option<String> {
    let st = SyncState::load(dir);
    (!st.device_id.is_empty()).then_some(st.device_id)
}

/// Exclusive lock on the state directory while syncing.
pub struct StateLock {
    _file: File,
}

impl StateLock {
    pub async fn acquire(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(anyhow!("another wisprcheap process is syncing; try again later"));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }
}
