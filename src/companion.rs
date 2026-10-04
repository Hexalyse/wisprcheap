//! Local, versioned interface for the optional desktop companion. No GUI dependency lives here.
//! Disk work is dispatched off the dictation actor. All writes use validated YAML patches.

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{ConfigArgs, load_config_lenient, validate_config_text};
use crate::history::{HistoryEntry, read_history};
use crate::yaml_edit::YamlText;

pub const PREFIX: &str = "ui-v1 ";
pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Secret,
    Number,
    Keys,
    Bool,
    LongText,
}

pub struct SettingSpec {
    pub path: &'static str,
    pub section: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub hint: &'static str,
}

macro_rules! fields {
    ($(($section:literal, $path:literal, $label:literal, $kind:ident, $hint:literal)),* $(,)?) => {
        pub const SETTINGS: &[SettingSpec] = &[$(SettingSpec { section: $section, path: $path, label: $label, kind: FieldKind::$kind, hint: $hint }),*];
    };
}

fields![
    (
        "General",
        "hotkey.keys",
        "Dictation shortcut",
        Keys,
        "Key names separated by +, for example Ctrl + Win"
    ),
    (
        "General",
        "hotkey.commandKeys",
        "Command shortcut",
        Keys,
        "Leave empty to disable command mode"
    ),
    (
        "General",
        "hotkey.addWordKeys",
        "Add-word shortcut",
        Keys,
        "Adds the selected word to your dictionary"
    ),
    (
        "General",
        "hotkey.handsFreeDoubleTap",
        "Double-tap for hands-free recording",
        Bool,
        "Tap again to finish"
    ),
    (
        "General",
        "hotkey.cancelOnOtherKey",
        "Cancel when another key is pressed",
        Bool,
        "Keeps system shortcuts usable"
    ),
    (
        "General",
        "hotkey.tapMaxMs",
        "Maximum tap duration (ms)",
        Number,
        "A longer press records normally"
    ),
    (
        "General",
        "hotkey.doubleTapWindowMs",
        "Double-tap interval (ms)",
        Number,
        "Time between the two taps"
    ),
    (
        "General",
        "overlay.enabled",
        "Show the native recording overlay",
        Bool,
        "The overlay remains native; this window is independent"
    ),
    (
        "General",
        "sounds.enabled",
        "Play sound cues",
        Bool,
        "Recording, processing and error feedback"
    ),
    (
        "General",
        "sounds.volume",
        "Sound volume",
        Number,
        "From 0 to 1"
    ),
    (
        "General",
        "notifications.errors",
        "Show error notifications",
        Bool,
        "Click a notification to open the log"
    ),
    (
        "Recording",
        "recording.device",
        "Microphone",
        Text,
        "default, a device index, or part of its name"
    ),
    (
        "Recording",
        "recording.minDurationMs",
        "Minimum recording length (ms)",
        Number,
        "Shorter recordings are discarded"
    ),
    (
        "Recording",
        "recording.tailMs",
        "Audio after releasing the shortcut (ms)",
        Number,
        "Helps keep the last word intact"
    ),
    (
        "Recording",
        "recording.maxDurationSec",
        "Maximum recording length (seconds)",
        Number,
        "Recording stops at this limit"
    ),
    (
        "Recording",
        "recording.silenceThresholdDb",
        "Silence threshold (dBFS)",
        Number,
        "Skip requests for audio quieter than this level"
    ),
    (
        "Speech-to-text",
        "transcription.provider",
        "Provider",
        Text,
        "elevenlabs or openai (including compatible endpoints)"
    ),
    (
        "Speech-to-text",
        "transcription.language",
        "Spoken language",
        Text,
        "auto, en, fr, de, or another supported language code"
    ),
    (
        "Speech-to-text",
        "transcription.timeoutMs",
        "Request timeout (ms)",
        Number,
        "Applies to speech-to-text requests"
    ),
    (
        "Speech-to-text",
        "transcription.elevenlabs.apiKey",
        "ElevenLabs API key",
        Secret,
        "Empty inherits ELEVENLABS_API_KEY; ${VAR} references are preserved"
    ),
    (
        "Speech-to-text",
        "transcription.elevenlabs.model",
        "ElevenLabs model",
        Text,
        "For example scribe_v2"
    ),
    (
        "Speech-to-text",
        "transcription.elevenlabs.baseUrl",
        "ElevenLabs endpoint",
        Text,
        "Provider base URL"
    ),
    (
        "Speech-to-text",
        "transcription.elevenlabs.keyterms",
        "Send dictionary as keyterms",
        Bool,
        "ElevenLabs charges a keyterms surcharge"
    ),
    (
        "Speech-to-text",
        "transcription.elevenlabs.noVerbatim",
        "Let ElevenLabs remove filler words",
        Bool,
        "Use non-verbatim transcription"
    ),
    (
        "Speech-to-text",
        "transcription.openai.apiKey",
        "OpenAI-compatible transcription key",
        Secret,
        "Empty inherits OPENAI_API_KEY"
    ),
    (
        "Speech-to-text",
        "transcription.openai.model",
        "OpenAI-compatible transcription model",
        Text,
        "For example gpt-4o-transcribe"
    ),
    (
        "Speech-to-text",
        "transcription.openai.baseUrl",
        "OpenAI-compatible transcription endpoint",
        Text,
        "Include /v1 where required"
    ),
    (
        "Speech-to-text",
        "transcription.openai.prompt",
        "Transcription context",
        LongText,
        "Dictionary terms are appended automatically"
    ),
    (
        "Cleanup",
        "polish.enabled",
        "Clean up dictations",
        Bool,
        "Remove filler words and fix punctuation"
    ),
    (
        "Cleanup",
        "polish.apiKey",
        "Cleanup API key",
        Secret,
        "Empty inherits OPENAI_API_KEY"
    ),
    (
        "Cleanup",
        "polish.model",
        "Cleanup model",
        Text,
        "Any model supported by your endpoint"
    ),
    (
        "Cleanup",
        "polish.baseUrl",
        "Cleanup endpoint",
        Text,
        "OpenAI-compatible Chat Completions endpoint"
    ),
    (
        "Cleanup",
        "polish.reasoningEffort",
        "Reasoning effort",
        Text,
        "none, low, medium, high; null disables the parameter"
    ),
    (
        "Cleanup",
        "polish.temperature",
        "Temperature",
        Number,
        "0 to 2, or null to omit the parameter"
    ),
    (
        "Cleanup",
        "polish.minWords",
        "Skip cleanup below this word count",
        Number,
        "0 always uses cleanup; translation still uses its model"
    ),
    (
        "Cleanup",
        "polish.timeoutMs",
        "Cleanup timeout (ms)",
        Number,
        "Falls back to the raw transcript on failure"
    ),
    (
        "Cleanup",
        "polish.instructions",
        "Cleanup instructions",
        LongText,
        "Instructions sent with each transcript"
    ),
    (
        "Command",
        "command.apiKey",
        "Command API key",
        Secret,
        "Empty inherits the cleanup key"
    ),
    (
        "Command",
        "command.model",
        "Command model",
        Text,
        "Empty inherits the cleanup model"
    ),
    (
        "Command",
        "command.baseUrl",
        "Command endpoint",
        Text,
        "Empty inherits the cleanup endpoint"
    ),
    (
        "Command",
        "command.reasoningEffort",
        "Command reasoning effort",
        Text,
        "Empty inherits cleanup; null disables it"
    ),
    (
        "Command",
        "command.temperature",
        "Command temperature",
        Number,
        "Empty inherits cleanup; null disables it"
    ),
    (
        "Command",
        "command.timeoutMs",
        "Command timeout (ms)",
        Number,
        "Applies to spoken commands"
    ),
    (
        "Translation",
        "translation.apiKey",
        "Translation API key",
        Secret,
        "Empty inherits the cleanup key"
    ),
    (
        "Translation",
        "translation.model",
        "Translation model",
        Text,
        "Empty inherits the cleanup model"
    ),
    (
        "Translation",
        "translation.baseUrl",
        "Translation endpoint",
        Text,
        "Empty inherits the cleanup endpoint"
    ),
    (
        "Translation",
        "translation.reasoningEffort",
        "Translation reasoning effort",
        Text,
        "Empty inherits cleanup; null disables it"
    ),
    (
        "Translation",
        "translation.temperature",
        "Translation temperature",
        Number,
        "Empty inherits cleanup; null disables it"
    ),
    (
        "Translation",
        "translation.timeoutMs",
        "Translation timeout (ms)",
        Number,
        "Applies when a translation pair is selected"
    ),
    (
        "Output & privacy",
        "output.paste",
        "Paste into the focused application",
        Bool,
        "Otherwise the result is copied to the clipboard"
    ),
    (
        "Output & privacy",
        "output.restoreClipboard",
        "Restore the previous clipboard after pasting",
        Bool,
        "Restores plain-text clipboard content"
    ),
    (
        "Output & privacy",
        "output.trailingSpace",
        "Append a space to dictations",
        Bool,
        "Helps consecutive dictations stay separated"
    ),
    (
        "Output & privacy",
        "history.enabled",
        "Keep dictation history",
        Bool,
        "Includes raw and cleaned-up text"
    ),
    (
        "Output & privacy",
        "history.path",
        "History file",
        Text,
        "Relative to your config folder, or an absolute path"
    ),
    (
        "Output & privacy",
        "history.saveFailedAudio",
        "Save audio when transcription fails",
        Bool,
        "Allows recovery of failed recordings"
    ),
    (
        "Output & privacy",
        "history.failedAudioDir",
        "Failed recordings folder",
        Text,
        "Relative to your config folder, or an absolute path"
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub path: String,
    pub revision: String,
    pub fields: BTreeMap<String, Value>,
    pub dictionary: Value,
    pub pairs: Value,
    pub prices: Value,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    /// None removes an override; Some(null) is an explicit YAML null.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "change_value"
    )]
    pub value: Option<Value>,
}

fn change_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub running: bool,
    pub paused: bool,
    pub recording: bool,
    pub pending: usize,
    pub label: String,
    pub last_text: Option<String>,
    pub retry_available: bool,
    pub sync: Option<String>,
    pub translation: Option<String>,
    pub translations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "camelCase")]
pub enum Request {
    Config,
    Save {
        revision: String,
        changes: Vec<Change>,
    },
    Activity {
        query: String,
        errors_only: bool,
        days: Option<u32>,
        #[serde(default)]
        scope: ActivityScope,
    },
    Status,
    Devices,
    Log,
    Pause {
        paused: bool,
    },
    Translate {
        index: i32,
    },
    CopyLast,
    RetryFailed,
    SyncNow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", content = "data", rename_all = "camelCase")]
pub enum Reply {
    Config(ConfigDocument),
    Activity(Activity),
    Status(RuntimeStatus),
    Devices(Vec<String>),
    Log(Vec<String>),
    Ok,
    Error(String),
}

impl Reply {
    pub fn json(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|_| {
            "{\"reply\":\"error\",\"data\":\"Could not encode response\"}".into()
        })
    }
}

fn revision(source: &str, env_files: &[PathBuf]) -> Result<String> {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    for file in env_files {
        file.hash(&mut hash);
        match std::fs::read(file) {
            Ok(bytes) => {
                1_u8.hash(&mut hash);
                bytes.hash(&mut hash);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0_u8.hash(&mut hash),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(format!("{:016x}", hash.finish()))
}

fn source_at(path: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

fn value_at<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(doc, |value, key| value.get(key))
}

pub fn config_document(args: &ConfigArgs) -> Result<ConfigDocument> {
    let _edit = crate::config::EDIT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    config_document_unlocked(args)
}

fn config_document_unlocked(args: &ConfigArgs) -> Result<ConfigDocument> {
    let (loaded, warnings) = load_config_lenient(args)?;
    let path = loaded
        .config_path
        .clone()
        .unwrap_or_else(|| loaded.base_dir.join("config.yaml"));
    let source = source_at(&path)?;
    let raw: Value = serde_yaml::from_str(&source)?;
    let defaults = serde_json::to_value(&loaded.config)?;
    let mut fields = BTreeMap::new();
    for spec in SETTINGS {
        // Never expand secrets out of .env into the UI or write them back as incidental edits.
        let mut default = if spec.kind == FieldKind::Secret {
            Value::Null
        } else {
            value_at(&defaults, spec.path)
                .cloned()
                .unwrap_or(Value::Null)
        };
        if default.is_null()
            && (spec.path.starts_with("command.") || spec.path.starts_with("translation."))
        {
            default = Value::String(String::new());
        }
        fields.insert(
            spec.path.into(),
            value_at(&raw, spec.path).cloned().unwrap_or(default),
        );
    }
    Ok(ConfigDocument {
        path: path.to_string_lossy().into_owned(),
        revision: revision(&source, &loaded.env_files)?,
        fields,
        dictionary: raw
            .get("dictionary")
            .filter(|v| v.is_array())
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        pairs: raw
            .pointer("/translation/pairs")
            .filter(|v| v.is_array())
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        prices: raw
            .pointer("/pricing/overrides")
            .filter(|v| v.is_array())
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        warnings,
    })
}

pub fn save_config(
    args: &ConfigArgs,
    expected_revision: &str,
    changes: &[Change],
) -> Result<ConfigDocument> {
    let _edit = crate::config::EDIT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let current = config_document_unlocked(args)?;
    if current.revision != expected_revision {
        bail!(
            "Settings changed in the config file or through sync. Reload settings before saving; your edits have not been written."
        );
    }
    let path = PathBuf::from(&current.path);
    let original = source_at(&path)?;
    let mut yaml = YamlText::new(if original.is_empty() {
        crate::config::EXAMPLE_CONFIG.to_string()
    } else {
        original
    });
    let mut seen = HashSet::new();
    for change in changes {
        if !SETTINGS.iter().any(|s| s.path == change.path)
            && !["dictionary", "translation.pairs", "pricing.overrides"]
                .contains(&change.path.as_str())
        {
            bail!("Unsupported setting: {}", change.path);
        }
        if !seen.insert(&change.path) {
            bail!("Duplicate setting: {}", change.path);
        }
        let keys: Vec<_> = change.path.split('.').collect();
        match &change.value {
            Some(value) => yaml.set(&keys, &serde_yaml::to_value(value)?)?,
            None => yaml.remove(&keys)?,
        }
    }
    // Missing keys are allowed so first-time setup can be completed in stages. Schema/range errors aren't.
    validate_config_text(args, yaml.as_str())?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Config has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    if path.exists() {
        file.as_file()
            .set_permissions(std::fs::metadata(&path)?.permissions())?;
    }
    file.write_all(yaml.as_str().as_bytes())?;
    file.as_file().sync_all()?;
    if config_document_unlocked(args)?.revision != expected_revision {
        bail!(
            "Settings changed while saving. Reload before trying again; your edits have not been written."
        );
    }
    file.persist(&path).map_err(|e| e.error)?;
    config_document_unlocked(args)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub recordings: usize,
    pub dictations: usize,
    pub commands: usize,
    pub words: usize,
    pub audio_minutes: f64,
    pub stt_usd: f64,
    pub llm_usd: f64,
    pub total_usd: f64,
    pub failures: usize,
    pub fallbacks: usize,
    pub unknown_prices: usize,
    pub median_ms: u64,
    pub p95_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatRow {
    pub label: String,
    pub words: usize,
    pub count: usize,
    pub usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub scope: ActivityScope,
    pub source: String,
    pub totals: Totals,
    pub daily: Vec<StatRow>,
    pub months: Vec<StatRow>,
    pub models: Vec<StatRow>,
    pub entries: Vec<HistoryEntry>,
    pub matches: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityScope {
    #[default]
    CurrentDevice,
    AllDevices,
}

impl std::fmt::Display for ActivityScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CurrentDevice => "Current device",
            Self::AllDevices => "All devices",
        })
    }
}

pub fn activity(
    entries: &[HistoryEntry],
    query: &str,
    errors_only: bool,
    days: Option<u32>,
    scope: ActivityScope,
    device_id: Option<&str>,
) -> Activity {
    let now = Local::now();
    let cutoff = days.map(|d| now.date_naive() - Duration::days(d.saturating_sub(1) as i64));
    let mut totals = Totals::default();
    let mut daily = BTreeMap::<String, StatRow>::new();
    let mut months = BTreeMap::<String, StatRow>::new();
    let mut models = BTreeMap::<String, StatRow>::new();
    let mut timings = Vec::new();
    let query = query.to_lowercase();
    let mut matches = Vec::new();
    for entry in entries {
        if scope == ActivityScope::CurrentDevice
            && entry
                .device
                .as_deref()
                .is_some_and(|id| Some(id) != device_id)
        {
            continue;
        }
        let date = DateTime::parse_from_rfc3339(&entry.ts)
            .ok()
            .map(|d| d.with_timezone(&Local).date_naive());
        if cutoff.is_some_and(|c| date.is_none_or(|d| d < c)) {
            continue;
        }
        totals.recordings += 1;
        totals.failures += usize::from(entry.error.is_some());
        totals.fallbacks += usize::from(entry.polish.as_ref().is_some_and(|p| p.error.is_some()));
        totals.audio_minutes += entry.duration_sec / 60.0;
        let cost = entry.cost_usd.total.unwrap_or_else(|| {
            entry.cost_usd.transcription.unwrap_or(0.0) + entry.cost_usd.polish.unwrap_or(0.0)
        });
        totals.stt_usd += entry.cost_usd.transcription.unwrap_or(0.0);
        totals.llm_usd += entry.cost_usd.polish.unwrap_or(0.0);
        totals.total_usd += cost;
        if entry.error.is_none() && entry.words > 0 {
            totals.words += entry.words;
            if entry.mode.as_deref() == Some("command") {
                totals.commands += 1;
            } else {
                totals.dictations += 1;
            }
            totals.unknown_prices += usize::from(
                entry.cost_usd.transcription.is_none()
                    || (entry.polish.as_ref().is_some_and(|p| p.error.is_none())
                        && entry.cost_usd.polish.is_none()),
            );
            timings.push(entry.transcription.ms + entry.polish.as_ref().map(|p| p.ms).unwrap_or(0));
        }
        let add = |map: &mut BTreeMap<String, StatRow>, label: String| {
            let row = map.entry(label.clone()).or_insert(StatRow {
                label,
                words: 0,
                count: 0,
                usd: 0.0,
            });
            row.words += entry.words;
            row.count += 1;
            row.usd += cost;
        };
        if let Some(date) = date {
            if date >= now.date_naive() - Duration::days(29) {
                add(&mut daily, date.format("%Y-%m-%d").to_string());
            }
            add(&mut months, date.format("%Y-%m").to_string());
        }
        let stt = models
            .entry(entry.transcription.model.clone())
            .or_insert(StatRow {
                label: entry.transcription.model.clone(),
                words: 0,
                count: 0,
                usd: 0.0,
            });
        stt.count += 1;
        stt.words += entry.words;
        stt.usd += entry.cost_usd.transcription.unwrap_or(0.0);
        if let Some(polish) = &entry.polish {
            let llm = models.entry(polish.model.clone()).or_insert(StatRow {
                label: polish.model.clone(),
                words: 0,
                count: 0,
                usd: 0.0,
            });
            llm.count += 1;
            llm.usd += entry.cost_usd.polish.unwrap_or(0.0);
        }
        if errors_only
            && entry.error.is_none()
            && entry.polish.as_ref().is_none_or(|p| p.error.is_none())
        {
            continue;
        }
        if !query.is_empty()
            && ![
                &entry.text,
                &entry.raw,
                &entry.transcription.model,
                entry.error.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|s| s.to_lowercase().contains(&query))
        {
            continue;
        }
        matches.push(entry.clone());
    }
    timings.sort_unstable();
    if !timings.is_empty() {
        totals.median_ms = timings[(timings.len() - 1) / 2];
        totals.p95_ms = timings[(timings.len() * 95).div_ceil(100).saturating_sub(1)];
    }
    let count = matches.len();
    // Synced/imported entries need not arrive in chronological order.
    matches.sort_by_cached_key(|entry| DateTime::parse_from_rfc3339(&entry.ts).ok());
    let entries = matches.into_iter().rev().take(200).collect();
    for offset in (0..30).rev() {
        let date = now.date_naive() - Duration::days(offset);
        if cutoff.is_some_and(|c| date < c) {
            continue;
        }
        let label = date.format("%Y-%m-%d").to_string();
        daily.entry(label.clone()).or_insert(StatRow {
            label,
            words: 0,
            count: 0,
            usd: 0.0,
        });
    }
    Activity {
        scope,
        source: String::new(),
        totals,
        daily: daily.into_values().collect(),
        months: months.into_values().rev().collect(),
        models: models.into_values().collect(),
        entries,
        matches: count,
    }
}

pub fn disk_request(args: &ConfigArgs, request: Request) -> Reply {
    let result = (|| -> Result<Reply> {
        Ok(match request {
            Request::Config => Reply::Config(config_document(args)?),
            Request::Save { revision, changes } => {
                Reply::Config(save_config(args, &revision, &changes)?)
            }
            Request::Devices => Reply::Devices(crate::recorder::list_input_devices()),
            Request::Log => Reply::Log(crate::logging::last_session_lines(
                &crate::logging::log_file_path(&crate::config::resolve_base_dir(args)),
                500,
            )),
            _ => bail!("Start dictation to use this action"),
        })
    })();
    result.unwrap_or_else(|e| Reply::Error(e.to_string()))
}

/// Fetch account history only when requested. Never changes the sync cursor, config, or history file.
pub async fn handle_request(args: ConfigArgs, request: Request) -> Reply {
    handle_request_at(args, request, crate::sync::state::default_state_dir()).await
}

pub async fn handle_request_at(args: ConfigArgs, request: Request, state_dir: PathBuf) -> Reply {
    let Request::Activity {
        query,
        errors_only,
        days,
        scope,
    } = request
    else {
        return tokio::task::spawn_blocking(move || disk_request(&args, request))
            .await
            .unwrap_or_else(|e| Reply::Error(e.to_string()));
    };
    let result = async {
        let (loaded, mut entries, device_id) = tokio::task::spawn_blocking(move || {
            let (loaded, _) = load_config_lenient(&args)?;
            let entries = read_history(&crate::paths::resolve(
                &loaded.base_dir,
                &loaded.config.history.path,
            ));
            let device_id = crate::sync::state::device_id(&state_dir);
            Ok::<_, anyhow::Error>((loaded, entries, device_id))
        })
        .await??;
        let source = if scope == ActivityScope::AllDevices && loaded.config.sync.enabled() {
            // Fit within the local IPC deadline, and keep network work off the dictation actor.
            entries = tokio::time::timeout(
                std::time::Duration::from_secs(12),
                account_history(&loaded.config.sync, entries),
            )
            .await
            .map_err(|_| {
                anyhow!("Loading all-device history timed out. Check sync and refresh to retry.")
            })??;
            "All synced devices, including local recordings waiting to sync."
        } else if scope == ActivityScope::AllDevices {
            "Sync is not connected; showing only history available on this computer."
        } else {
            "Recordings from this computer, including history recorded before pairing."
        };
        let mut data = activity(
            &entries,
            &query,
            errors_only,
            days,
            scope,
            device_id.as_deref(),
        );
        data.source = source.into();
        Ok::<_, anyhow::Error>(data)
    }
    .await;
    match result {
        Ok(data) => Reply::Activity(data),
        Err(e) => Reply::Error(e.to_string()),
    }
}

fn history_id(entry: &HistoryEntry, device_id: &str) -> String {
    entry
        .id
        .as_deref()
        .filter(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| {
            crate::sync::engine::legacy_entry_id(
                entry.device.as_deref().unwrap_or(device_id),
                &entry.ts,
            )
        })
}

async fn account_history(
    sync: &crate::config::SyncConfig,
    local: Vec<HistoryEntry>,
) -> Result<Vec<HistoryEntry>> {
    use wisprcheap_sync::{crypto::DataKey, profile::KIND_HISTORY};
    let key = DataKey::import(&sync.key).map_err(|_| {
        anyhow!("Unlock sync with `wisprcheap sync unlock` before loading all-device history.")
    })?;
    let server = crate::sync::client::normalize_server(&sync.server).map_err(|e| anyhow!(e))?;
    let client = crate::sync::client::Client::new(&server, &sync.token);
    let me = client.me().await?;
    if me
        .keyring
        .as_ref()
        .is_some_and(|ring| ring.key_id != key.key_id())
    {
        bail!("The sync encryption key changed. Run `wisprcheap sync unlock`, then refresh.");
    }
    let mut entries = BTreeMap::new();
    let mut deleted = HashSet::new();
    let mut cursor = 0;
    loop {
        let page = client.pull(cursor, 500, false).await?;
        for change in page.changes.into_iter().filter(|c| c.kind == KIND_HISTORY) {
            if change.deleted {
                entries.remove(&change.id);
                deleted.insert(change.id);
                continue;
            }
            let payload = change
                .payload
                .as_deref()
                .ok_or_else(|| anyhow!("Synced history is missing its encrypted content."))?;
            let value = key.decrypt_record(&me.user.id, KIND_HISTORY, &change.id, payload)
                .map_err(|_| anyhow!("Could not decrypt synced history. Check the sync key with `wisprcheap sync unlock`."))?;
            let mut entry: HistoryEntry = serde_json::from_value(value)?;
            entry.id = Some(change.id.clone());
            if change.device.is_some() {
                entry.device = change.device;
            }
            deleted.remove(&change.id);
            entries.insert(change.id, entry);
        }
        if !page.has_more {
            break;
        }
        if page.next_since <= cursor {
            bail!("The sync server did not advance its history cursor.");
        }
        cursor = page.next_since;
    }
    // UUIDs and the sync engine's legacy IDs prevent double-counting uploads and downloaded copies.
    for entry in local {
        let id = history_id(&entry, &me.device.id);
        if deleted.contains(&id) {
            continue;
        }
        if entry.device.as_deref().is_none_or(|id| id == me.device.id) {
            entries.insert(id, entry);
        } else {
            entries.entry(id).or_insert(entry);
        }
    }
    Ok(entries.into_values().collect())
}

/// An absent instance permits offline setup; a failed request to an existing instance never writes offline.
pub async fn request(args: ConfigArgs, request: Request) -> Reply {
    let command = format!("{PREFIX}{}", serde_json::to_string(&request).unwrap());
    match crate::instance::send_command(&command, std::time::Duration::from_secs(15)).await {
        Some(reply) => serde_json::from_str(&reply).unwrap_or_else(|_| Reply::Error("The running app could not answer. Check that the app and companion are the same version, then retry.".into())),
        None if matches!(request, Request::Status) => Reply::Status(RuntimeStatus::default()),
        None => handle_request(args, request).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, ConfigArgs) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.yaml");
        std::fs::write(&file, "# preserve this comment\npolish:\n  enabled: false\n  model: ${MY_MODEL}\nunknown: keep-me\n").unwrap();
        std::fs::write(dir.path().join(".env"), "MY_MODEL=custom-model\n").unwrap();
        (
            dir,
            ConfigArgs {
                config: Some(file.to_string_lossy().into_owned()),
                isolated: true,
            },
        )
    }

    #[test]
    fn edits_preserve_comments_references_and_unknown_settings() {
        let (_dir, args) = fixture();
        let before = config_document(&args).unwrap();
        assert_eq!(before.fields["polish.model"], "${MY_MODEL}");
        let saved = save_config(
            &args,
            &before.revision,
            &[Change {
                path: "sounds.volume".into(),
                value: Some(serde_json::json!(0.5)),
            }],
        )
        .unwrap();
        let text = std::fs::read_to_string(saved.path).unwrap();
        assert!(text.contains("# preserve this comment"));
        assert!(text.contains("model: ${MY_MODEL}"));
        assert!(text.contains("unknown: keep-me"));
    }

    #[test]
    fn stale_and_invalid_changes_do_not_write() {
        let (dir, args) = fixture();
        let before = config_document(&args).unwrap();
        let original = std::fs::read_to_string(&before.path).unwrap();
        let invalid = Change {
            path: "sounds.volume".into(),
            value: Some(serde_json::json!(2)),
        };
        assert!(save_config(&args, &before.revision, &[invalid]).is_err());
        assert_eq!(std::fs::read_to_string(&before.path).unwrap(), original);
        std::fs::write(dir.path().join(".env"), "MY_MODEL=changed\n").unwrap();
        assert!(save_config(&args, &before.revision, &[]).is_err());
    }

    #[test]
    fn activity_includes_failures_unknown_prices_and_stage_latency() {
        let mut success = HistoryEntry {
            ts: Local::now().to_rfc3339(),
            words: 20,
            duration_sec: 6.0,
            ..Default::default()
        };
        success.transcription.ms = 500;
        let failure = HistoryEntry {
            ts: success.ts.clone(),
            error: Some("network".into()),
            duration_sec: 3.0,
            ..Default::default()
        };
        let stats = activity(
            &[success, failure],
            "",
            false,
            Some(7),
            ActivityScope::CurrentDevice,
            None,
        );
        assert_eq!(stats.totals.failures, 1);
        assert_eq!(stats.totals.dictations, 1);
        assert_eq!(stats.totals.unknown_prices, 1);
        assert_eq!(stats.totals.median_ms, 500);
        assert!((stats.totals.audio_minutes - 0.15).abs() < 1e-10);
    }

    #[test]
    fn device_scope_filters_every_summary_and_history_before_search() {
        let mut local = HistoryEntry {
            ts: Local::now().to_rfc3339(),
            words: 10,
            duration_sec: 60.0,
            ..Default::default()
        };
        local.transcription.model = "local-model".into();
        local.transcription.ms = 100;
        local.cost_usd.transcription = Some(0.01);
        local.cost_usd.total = Some(0.01);
        let mut paired = local.clone();
        paired.device = Some("this-device".into());
        let mut remote = local.clone();
        remote.device = Some("other-device".into());
        remote.transcription.model = "remote-model".into();
        remote.error = Some("remote failure".into());
        let entries = [local, paired, remote];
        let current = activity(
            &entries,
            "",
            false,
            Some(7),
            ActivityScope::CurrentDevice,
            Some("this-device"),
        );
        assert_eq!(
            (
                current.totals.recordings,
                current.totals.words,
                current.matches
            ),
            (2, 20, 2)
        );
        assert_eq!(current.totals.failures, 0);
        assert_eq!(current.totals.audio_minutes, 2.0);
        assert!((current.totals.total_usd - 0.02).abs() < 1e-10);
        assert_eq!(current.daily.iter().map(|r| r.count).sum::<usize>(), 2);
        assert_eq!(current.months[0].count, 2);
        assert_eq!(current.models.len(), 1);
        assert_eq!(current.models[0].label, "local-model");
        let errors = activity(
            &entries,
            "remote",
            true,
            None,
            ActivityScope::CurrentDevice,
            Some("this-device"),
        );
        assert_eq!(errors.totals.recordings, 2);
        assert_eq!(errors.matches, 0);
        let all = activity(
            &entries,
            "remote",
            true,
            None,
            ActivityScope::AllDevices,
            Some("this-device"),
        );
        assert_eq!(all.totals.recordings, 3);
        assert_eq!(all.totals.failures, 1);
        assert_eq!(all.matches, 1);
        assert_eq!(all.entries[0].device.as_deref(), Some("other-device"));
        // Without pairing, only untagged history belongs to this device.
        assert_eq!(
            activity(
                &entries,
                "",
                false,
                None,
                ActivityScope::CurrentDevice,
                None
            )
            .totals
            .recordings,
            1
        );
    }
}
