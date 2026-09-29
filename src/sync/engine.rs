//! One sync run (SPEC.md section 8): local edits → outbox, pull, merge and write back, push,
//! history upload/download, statistics.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wisprcheap_sync::crypto::DataKey;
use wisprcheap_sync::hlc::{Clock, Hlc};
use wisprcheap_sync::profile::{KIND_DICT, KIND_HISTORY, KIND_PAIR, KIND_PRICE, KIND_SECRET};
use wisprcheap_sync::protocol::{Change, MonthStats, PushStatus};
use wisprcheap_sync::stats::HistoryStats;

use crate::config::{ConfigArgs, LoadedConfig, SyncHistoryMode, load_config_lenient};
use crate::sync::client::{ApiError, Client, normalize_server};
use crate::sync::local::{self, Incoming};
use crate::sync::state::{Snap, StateLock, SyncState, record_key, split_key};

const PAGE: usize = 500;
const MAX_PAYLOAD: usize = 64 * 1024;

/// Where a sync run reads its config and keeps its state.
#[derive(Debug, Clone)]
pub struct SyncContext {
    pub config_args: ConfigArgs,
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum SyncError {
    /// No `sync.server` / `sync.token`.
    Disabled,
    /// The token was revoked.
    Unauthorized,
    /// The data key is missing or outdated: `wisprcheap sync unlock`.
    NeedsKey(String),
    /// Server unreachable (retried with backoff).
    Network(String),
    /// Error answer from the server (retried with backoff).
    Server(String),
    /// Config or file problem on this device.
    Local(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::Disabled => write!(f, "sync is not set up (see `wisprcheap sync pair`)"),
            SyncError::Unauthorized => write!(
                f,
                "this device was disconnected from the server; pair it again with `wisprcheap sync pair`"
            ),
            SyncError::NeedsKey(m) | SyncError::Network(m) | SyncError::Server(m) | SyncError::Local(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for SyncError {}

impl From<ApiError> for SyncError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Unauthorized => SyncError::Unauthorized,
            ApiError::Network(_) => SyncError::Network(e.to_string()),
            ApiError::Status { .. } => SyncError::Server(e.to_string()),
        }
    }
}

fn local_err(e: impl fmt::Display) -> SyncError {
    SyncError::Local(e.to_string())
}

/// What a sync run did.
#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    pub first: bool,
    /// Records received (history excluded).
    pub pulled: usize,
    /// Local changes made from remote records, per kind.
    pub applied: BTreeMap<String, usize>,
    /// Records stored by the server, per kind (history excluded).
    pub pushed: BTreeMap<String, usize>,
    pub uploaded: usize,
    pub downloaded: usize,
    /// This month, every device.
    pub month: Option<MonthStats>,
    pub devices: usize,
    pub device_id: String,
    pub warnings: Vec<String>,
}

impl SyncOutcome {
    /// `pushed 3, pulled 12` style summary for the log.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let applied: usize = self.applied.values().sum();
        let pushed: usize = self.pushed.values().sum();
        if pushed > 0 {
            parts.push(format!("sent {pushed} change(s)"));
        }
        if applied > 0 {
            parts.push(format!("applied {applied} change(s) from other devices"));
        }
        if self.uploaded > 0 {
            parts.push(format!("uploaded {} history entr(ies)", self.uploaded));
        }
        if self.downloaded > 0 {
            parts.push(format!("added {} history entr(ies) from other devices", self.downloaded));
        }
        if parts.is_empty() {
            "up to date".into()
        } else {
            parts.join(", ")
        }
    }
}

fn kind_label(kind: &str, n: usize) -> String {
    let (one, many) = match kind {
        "setting" => ("setting", "settings"),
        "secret" => ("API key", "API keys"),
        "dict" => ("dictionary term", "dictionary terms"),
        "pair" => ("translation pair", "translation pairs"),
        "price" => ("price", "prices"),
        _ => ("record", "records"),
    };
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "12 settings updated from the server, 3 dictionary terms uploaded" (first sync summary).
pub fn describe_counts(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(k, n)| kind_label(k, *n))
        .collect::<Vec<_>>()
        .join(", ")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hlc_ge(a: &str, b: &str) -> bool {
    match (Hlc::parse(a), Hlc::parse(b)) {
        (Some(a), Some(b)) => a >= b,
        _ => a >= b,
    }
}

/// Keyed hash of a value, for the snapshot (no plain secrets on disk).
fn value_hash(dk: &DataKey, v: &Value) -> String {
    dk.blind_id("hash", &v.to_string())
}

/// Id of a history entry written before sync (SPEC.md section 6).
pub fn legacy_entry_id(device: &str, ts: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("wisprcheap:{device}:{ts}").as_bytes(),
    )
    .to_string()
}

fn is_set_kind(kind: &str) -> bool {
    matches!(kind, KIND_DICT | KIND_PAIR | KIND_PRICE)
}

/// Compares the local profile with the snapshot: every difference is a local edit, queued with a
/// fresh HLC. Returns the number of changes queued.
fn diff_local(
    loaded: &LoadedConfig,
    dk: &DataKey,
    st: &mut SyncState,
    clock: &mut Clock,
    skip: &HashSet<String>,
) -> usize {
    let local = local::profile(loaded, dk);
    let mut records: Vec<(&String, &local::LocalRecord)> = local.iter().collect();
    records.sort_by_key(|(_, r)| r.order);
    let mut queued = 0;
    for (key, rec) in records {
        if skip.contains(key) {
            continue;
        }
        let hash = value_hash(dk, &rec.value);
        if st
            .snapshot
            .get(key)
            .is_some_and(|s| !s.deleted && s.hash == hash)
        {
            continue;
        }
        let hlc = clock.now(now_ms()).to_string();
        let payload = dk.encrypt_record(&st.user_id, rec.kind, &rec.id, &rec.value);
        st.queue(Change {
            seq: None,
            kind: rec.kind.to_string(),
            id: rec.id.clone(),
            hlc: hlc.clone(),
            deleted: false,
            payload: Some(payload),
            device: None,
            stats: None,
        });
        st.snapshot.insert(
            key.clone(),
            Snap {
                hlc,
                hash,
                deleted: false,
            },
        );
        queued += 1;
    }
    let gone: Vec<String> = st
        .snapshot
        .iter()
        .filter(|(k, s)| {
            !s.deleted
                && !local.contains_key(*k)
                && !skip.contains(*k)
                && split_key(k).is_some_and(|(kind, _)| is_set_kind(kind))
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in gone {
        let Some((kind, id)) = split_key(&key) else {
            continue;
        };
        let hlc = clock.now(now_ms()).to_string();
        st.queue(Change {
            seq: None,
            kind: kind.to_string(),
            id: id.to_string(),
            hlc: hlc.clone(),
            deleted: true,
            payload: None,
            device: None,
            stats: None,
        });
        st.snapshot.insert(
            key.clone(),
            Snap {
                hlc,
                hash: String::new(),
                deleted: true,
            },
        );
        queued += 1;
    }
    queued
}

fn load(args: &ConfigArgs) -> Result<LoadedConfig, SyncError> {
    load_config_lenient(args).map(|(l, _)| l).map_err(local_err)
}

/// One complete sync. Holds the state lock for its whole duration.
pub async fn sync_once(ctx: &SyncContext) -> Result<SyncOutcome, SyncError> {
    let _lock = StateLock::acquire(&ctx.state_dir).await.map_err(local_err)?;
    let loaded = load(&ctx.config_args)?;
    let sc = loaded.config.sync.clone();
    if !sc.enabled() {
        return Err(SyncError::Disabled);
    }
    let server = normalize_server(&sc.server).map_err(SyncError::Local)?;
    let dk = DataKey::import(&sc.key).map_err(|_| {
        SyncError::NeedsKey(
            "this device doesn't have the encryption key (sync.key); run `wisprcheap sync unlock`".into(),
        )
    })?;
    let mut st = SyncState::load(&ctx.state_dir);
    if st.server != server {
        st = SyncState {
            server: server.clone(),
            ..Default::default()
        };
    }
    if st.key_id != dk.key_id() {
        st.reset_data();
        st.key_id = dk.key_id();
    }
    let mut run = Run {
        ctx,
        client: Client::new(&server, &sc.token),
        dk,
        clock: Clock::new(if st.device_id.is_empty() {
            "dev_unknown".to_string()
        } else {
            st.device_id.clone()
        }),
        out: SyncOutcome::default(),
    };
    if let Some(h) = Hlc::parse(&st.hlc) {
        run.clock.observe(&h);
    }
    let result = run.run(&mut st, loaded).await;
    st.hlc = run.clock.last().to_string();
    if let Err(e) = st.save(&ctx.state_dir) {
        crate::warn!("[sync] can't save the sync state: {e}");
    }
    result.map(|()| run.out)
}

struct Run<'a> {
    ctx: &'a SyncContext,
    client: Client,
    dk: DataKey,
    clock: Clock,
    out: SyncOutcome,
}

impl Run<'_> {
    async fn run(&mut self, st: &mut SyncState, loaded: LoadedConfig) -> Result<(), SyncError> {
        let sc = loaded.config.sync.clone();
        // 0. Local edits since the last sync, before any network call (so an edit made offline gets
        //    an HLC close to when it was made).
        let pending_keys = |st: &SyncState| -> HashSet<String> {
            st.pending.iter().map(|c| record_key(&c.kind, &c.id)).collect()
        };
        if st.initialized && !st.device_id.is_empty() {
            let skip = pending_keys(st);
            diff_local(&loaded, &self.dk, st, &mut self.clock, &skip);
            let _ = st.save(&self.ctx.state_dir);
        }

        let me = self.client.me().await?;
        if st.device_id != me.device.id || st.user_id != me.user.id {
            // Paired again (or another account): start over.
            let (server, key_id) = (st.server.clone(), st.key_id.clone());
            *st = SyncState {
                server,
                key_id,
                ..Default::default()
            };
            st.device_id = me.device.id.clone();
            st.user_id = me.user.id.clone();
            self.clock = Clock::new(me.device.id.clone());
        }
        st.username = me.user.username.clone();
        st.device_name = me.device.name.clone();
        self.out.device_id = me.device.id.clone();
        match &me.keyring {
            None => {
                return Err(SyncError::NeedsKey(
                    "the encryption was reset on the server; run `wisprcheap sync unlock` to set a new sync passphrase".into(),
                ));
            }
            Some(k) if k.key_id != self.dk.key_id() => {
                return Err(SyncError::NeedsKey(
                    "the sync passphrase was reset on another device; run `wisprcheap sync unlock`".into(),
                ));
            }
            _ => {}
        }

        let history_on = sc.history != SyncHistoryMode::Off && loaded.config.history.enabled;
        let download = history_on && sc.history == SyncHistoryMode::Download;
        if download && st.history_mode != SyncHistoryMode::Download.as_str() {
            st.cursor = 0; // fetch the history entries skipped so far
        }
        st.history_mode = sc.history.as_str().to_string();
        let first = !st.initialized;
        self.out.first = first;

        // 1. Pull.
        let mut incoming: BTreeMap<String, Change> = BTreeMap::new();
        for c in std::mem::take(&mut st.pending) {
            incoming.insert(record_key(&c.kind, &c.id), c);
        }
        let mut history_in: Vec<Change> = Vec::new();
        loop {
            let page = self.client.pull(st.cursor, PAGE, !download).await?;
            for c in page.changes {
                if let Some(h) = Hlc::parse(&c.hlc) {
                    self.clock.observe(&h);
                }
                if c.kind == KIND_HISTORY {
                    if download && !c.deleted && c.device.as_deref() != Some(st.device_id.as_str()) {
                        history_in.push(c);
                    }
                    continue;
                }
                self.out.pulled += 1;
                let key = record_key(&c.kind, &c.id);
                if st.snapshot.get(&key).is_some_and(|s| hlc_ge(&s.hlc, &c.hlc)) {
                    continue; // already applied (or our own change)
                }
                incoming.insert(key, c);
            }
            st.cursor = page.next_since;
            if !page.has_more {
                break;
            }
        }
        // A queued local change and a remote one on the same record: the newer wins.
        incoming.retain(|key, c| {
            match st.outbox.iter().position(|o| &record_key(&o.kind, &o.id) == key) {
                Some(i) if hlc_ge(&st.outbox[i].hlc, &c.hlc) => false,
                Some(i) => {
                    st.outbox.remove(i);
                    true
                }
                None => true,
            }
        });

        // 2-4. Decrypt, merge (first sync) and write back.
        let changes: Vec<Change> = incoming.into_values().collect();
        let mut loaded = loaded;
        if self.apply(st, &mut loaded, changes, first)? {
            loaded = load(&self.ctx.config_args)?;
        }

        // 5. Local changes after the merge: on the first sync, what the server didn't have; later,
        //    values the write-back normalised.
        let skip = pending_keys(st);
        diff_local(&loaded, &self.dk, st, &mut self.clock, &skip);
        let _ = st.save(&self.ctx.state_dir);

        // 6. Push.
        let mut stale: Vec<Change> = Vec::new();
        while !st.outbox.is_empty() {
            let batch: Vec<Change> = st.outbox.iter().take(PAGE).cloned().collect();
            let resp = self.client.push(&batch).await?;
            for (sent, r) in batch.iter().zip(resp.results) {
                st.outbox
                    .retain(|o| !(o.kind == sent.kind && o.id == sent.id && o.hlc == sent.hlc));
                match r.status {
                    PushStatus::Applied | PushStatus::Exists => {
                        *self.out.pushed.entry(sent.kind.clone()).or_default() += 1;
                    }
                    PushStatus::Stale => {
                        if let Some(cur) = r.current {
                            if let Some(h) = Hlc::parse(&cur.hlc) {
                                self.clock.observe(&h);
                            }
                            stale.push(cur);
                        }
                    }
                    PushStatus::Rejected => {
                        let error = r.error.unwrap_or_default();
                        let key = record_key(&sent.kind, &sent.id);
                        if error == "clock_skew" {
                            // Retry once the clock is right.
                            st.snapshot.remove(&key);
                            self.warn("the server refused a change because this computer's clock is ahead; check the date and time".into());
                        } else {
                            self.warn(format!("the server refused {key}: {error}"));
                        }
                    }
                }
            }
        }
        if !stale.is_empty() {
            let mut loaded2 = load(&self.ctx.config_args)?;
            self.apply(st, &mut loaded2, stale, false)?;
        }

        // 7-8. History.
        if history_on {
            let file = crate::paths::resolve(&loaded.base_dir, &loaded.config.history.path);
            self.out.uploaded = self.upload_history(st, &file).await?;
            if download {
                self.out.downloaded = self.download_history(st, &file, history_in);
            }
        }

        // 9. This month's totals, every device.
        let now = chrono::Local::now();
        let month = now.format("%Y-%m").to_string();
        let offset = i64::from(now.offset().local_minus_utc() / 60);
        match self.client.stats(&month, &month, offset).await {
            Ok(stats) => {
                self.out.month = stats.months.into_iter().find(|m| m.month == month);
                self.out.devices = stats.devices.len();
            }
            Err(e) => self.warn(format!("statistics unavailable: {e}")),
        }

        st.initialized = true;
        st.last_sync = Some(crate::history::iso_timestamp(chrono::Utc::now()));
        Ok(())
    }

    fn warn(&mut self, message: String) {
        crate::warn!("[sync] {message}");
        self.out.warnings.push(message);
    }

    /// Decrypts remote records and writes them into the config. Returns whether files changed.
    fn apply(
        &mut self,
        st: &mut SyncState,
        loaded: &mut LoadedConfig,
        changes: Vec<Change>,
        first: bool,
    ) -> Result<bool, SyncError> {
        if changes.is_empty() {
            return Ok(false);
        }
        // Server order, so list items land in the order they were added.
        let mut changes = changes;
        changes.sort_by_key(|c| c.seq.unwrap_or(i64::MAX));
        let local0 = local::profile(loaded, &self.dk);
        let mut decoded: Vec<(Change, Option<Value>)> = Vec::new();
        for c in changes {
            let value = if c.deleted {
                None
            } else {
                let Some(payload) = &c.payload else {
                    continue;
                };
                match self.dk.decrypt_record(&st.user_id, &c.kind, &c.id, payload) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        self.warn(format!("can't decrypt {}/{}: {e}", c.kind, c.id));
                        continue;
                    }
                }
            };
            decoded.push((c, value));
        }
        if first {
            // Pairing a device that already has data: keep what the server only has as a deletion
            // or an empty key (it's uploaded right after instead).
            decoded.retain(|(c, v)| {
                let Some(local) = local0.get(&record_key(&c.kind, &c.id)) else {
                    return true;
                };
                match v {
                    None => false,
                    Some(Value::String(s)) if c.kind == KIND_SECRET && s.is_empty() => {
                        local.value.as_str().is_none_or(str::is_empty)
                    }
                    _ => true,
                }
            });
        }
        if decoded.is_empty() {
            return Ok(false);
        }
        let list: Vec<Incoming> = decoded
            .iter()
            .map(|(c, v)| Incoming {
                kind: c.kind.clone(),
                id: c.id.clone(),
                value: v.clone(),
            })
            .collect();
        let mut failed: HashSet<String> = HashSet::new();
        let mut files_changed = false;
        match local::apply(loaded, &self.dk, &list, first) {
            Ok(report) => {
                files_changed = report.files_changed;
                for (key, why) in report.failed {
                    self.warn(format!("can't write {key} into the config yet: {why}"));
                    failed.insert(key);
                }
                for (key, why) in report.ignored {
                    self.warn(format!("ignored {key} from another device: {why}"));
                }
                for (kind, n) in report.changed {
                    *self.out.applied.entry(kind).or_default() += n;
                }
            }
            Err(e) => {
                self.warn(format!("can't update the config: {e}"));
                failed.extend(list.iter().map(Incoming::key));
            }
        }
        for (c, v) in decoded {
            let key = record_key(&c.kind, &c.id);
            if failed.contains(&key) {
                st.pending.push(c);
                continue;
            }
            st.snapshot.insert(
                key,
                Snap {
                    hlc: c.hlc.clone(),
                    hash: v.as_ref().map(|v| value_hash(&self.dk, v)).unwrap_or_default(),
                    deleted: v.is_none(),
                },
            );
        }
        Ok(files_changed)
    }

    /// Builds the `history` record of one `history.jsonl` line (None: not ours, or unreadable).
    fn history_change(&mut self, st: &SyncState, line: &[u8]) -> Option<Change> {
        let mut entry: Value = serde_json::from_slice(line).ok()?;
        let obj = entry.as_object_mut()?;
        if let Some(d) = obj.get("device").and_then(Value::as_str)
            && d != st.device_id
        {
            return None; // another device's entry (downloaded)
        }
        let ts = obj.get("ts")?.as_str()?.to_string();
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| uuid::Uuid::parse_str(id).is_ok() && id.len() == 36)
            .map(str::to_lowercase)
            .unwrap_or_else(|| legacy_entry_id(&st.device_id, &ts));
        obj.insert("id".into(), Value::String(id.clone()));
        obj.insert("device".into(), Value::String(st.device_id.clone()));
        let stats = HistoryStats::from_entry(&entry)?;
        if let Err(e) = stats.validate() {
            self.warn(format!("history entry {ts} not uploaded: {e}"));
            return None;
        }
        let payload = self.dk.encrypt_record(&st.user_id, KIND_HISTORY, &id, &entry);
        if payload.len() > MAX_PAYLOAD {
            self.warn(format!("history entry {ts} is too large to upload"));
            return None;
        }
        Some(Change {
            seq: None,
            kind: KIND_HISTORY.into(),
            id,
            hlc: self.clock.now(now_ms()).to_string(),
            deleted: false,
            payload: Some(payload),
            device: None,
            stats: Some(stats),
        })
    }

    async fn push_history(&mut self, batch: &[Change]) -> Result<usize, SyncError> {
        if batch.is_empty() {
            return Ok(0);
        }
        let resp = self.client.push(batch).await?;
        let mut n = 0;
        for r in resp.results {
            match r.status {
                PushStatus::Applied => n += 1,
                PushStatus::Exists | PushStatus::Stale => {}
                PushStatus::Rejected => self.warn(format!(
                    "history entry {} refused: {}",
                    r.id,
                    r.error.unwrap_or_default()
                )),
            }
        }
        Ok(n)
    }

    /// Uploads the entries appended to `history.jsonl` since the last sync.
    async fn upload_history(&mut self, st: &mut SyncState, file: &Path) -> Result<usize, SyncError> {
        let name = file.to_string_lossy().into_owned();
        if st.history_file != name {
            st.history_file = name;
            st.history_offset = 0;
        }
        let bytes = match std::fs::read(file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(local_err(format!("can't read {}: {e}", file.display()))),
        };
        if bytes.len() as u64 > st.history_offset {
        } else if (bytes.len() as u64) < st.history_offset {
            st.history_offset = 0; // the file was replaced
        } else {
            return Ok(0);
        }
        let start = st.history_offset as usize;
        let rest = &bytes[start..];
        let complete = rest.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
        let mut uploaded = 0;
        let mut batch: Vec<Change> = Vec::new();
        let mut batch_bytes = 0;
        let mut pos = start;
        for line in rest[..complete].split_inclusive(|&b| b == b'\n') {
            pos += line.len();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Some(change) = self.history_change(st, line) {
                batch_bytes += change.payload.as_ref().map(String::len).unwrap_or(0) + 800;
                batch.push(change);
            }
            if batch.len() >= 200 || batch_bytes > 600_000 {
                uploaded += self.push_history(&batch).await?;
                batch.clear();
                batch_bytes = 0;
                st.history_offset = pos as u64;
            }
        }
        uploaded += self.push_history(&batch).await?;
        st.history_offset = (start + complete) as u64;
        Ok(uploaded)
    }

    /// Appends the other devices' entries to `history.jsonl` (`sync.history: download`).
    fn download_history(&mut self, st: &SyncState, file: &Path, changes: Vec<Change>) -> usize {
        if changes.is_empty() {
            return 0;
        }
        let known: HashSet<String> = std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.get("id").and_then(Value::as_str).map(String::from))
            .collect();
        let mut text = String::new();
        let mut added = 0;
        for c in changes {
            if known.contains(&c.id) {
                continue;
            }
            let Some(payload) = &c.payload else {
                continue;
            };
            let mut entry = match self.dk.decrypt_record(&st.user_id, KIND_HISTORY, &c.id, payload) {
                Ok(v) => v,
                Err(e) => {
                    self.warn(format!("can't decrypt history entry {}: {e}", c.id));
                    continue;
                }
            };
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("id".into(), Value::String(c.id.clone()));
                if let Some(d) = &c.device {
                    obj.entry("device").or_insert_with(|| Value::String(d.clone()));
                }
            }
            text.push_str(&entry.to_string());
            text.push('\n');
            added += 1;
        }
        if added == 0 {
            return 0;
        }
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
            f.write_all(text.as_bytes())
        })();
        match result {
            Ok(()) => added,
            Err(e) => {
                self.warn(format!("can't add history entries to {}: {e}", file.display()));
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ids_are_stable() {
        let a = legacy_entry_id("dev_a", "2026-09-29T12:34:56.789Z");
        assert_eq!(a, legacy_entry_id("dev_a", "2026-09-29T12:34:56.789Z"));
        assert_ne!(a, legacy_entry_id("dev_b", "2026-09-29T12:34:56.789Z"));
        assert_eq!(a.len(), 36);
        // Same as Python's uuid.uuid5(uuid.NAMESPACE_URL, …) and the Android port.
        assert_eq!(a, "e56fc249-9fa1-523b-b544-fc2e2da4f43f");
    }
}
