//! Configuration: YAML schema with defaults, `.env` loading, `${VAR}` interpolation, validation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Deserializer};

use crate::lang::{canonical_language, language_name};
use crate::paths::paths;

pub use wisprcheap_sync::profile::PriceValue;

pub const DEFAULT_POLISH_INSTRUCTIONS: &str = "Remove filler words, repeated starts, and abandoned phrases. When I correct myself, keep the final version. Fix punctuation and capitalization. Preserve my meaning, wording, names, and numbers.";

/// Written when the user opens or edits a config that doesn't exist yet.
pub const EXAMPLE_CONFIG: &str = include_str!("../config.example.yaml");

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

fn double_option<'de, T, D>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HotkeyConfig {
    /// Hold to dictate. All keys must be held. Generic names (Ctrl, Shift, Alt, Win) match left or right.
    pub keys: Vec<String>,
    /// Hold to speak an instruction that edits the selected text (or writes new text). [] disables it.
    pub command_keys: Vec<String>,
    /// Press to add the selected text to the dictionary. [] disables it.
    pub add_word_keys: Vec<String>,
    /// Double-tap the hotkey to record hands-free; tap once more to stop.
    pub hands_free_double_tap: bool,
    /// A press shorter than this counts as a "tap" (first half of a double-tap).
    pub tap_max_ms: u64,
    /// Max gap between the two taps of a double-tap.
    pub double_tap_window_ms: u64,
    /// Cancel the recording if another key is pressed while holding (e.g. Ctrl+Win+Left).
    pub cancel_on_other_key: bool,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            keys: vec!["Ctrl".into(), "Win".into()],
            command_keys: vec!["Ctrl".into(), "Win".into(), "Alt".into()],
            add_word_keys: vec!["Ctrl".into(), "Win".into(), "Shift".into()],
            hands_free_double_tap: true,
            tap_max_ms: 250,
            double_tap_window_ms: 350,
            cancel_on_other_key: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum DeviceSetting {
    Index(i64),
    Name(String),
}

impl DeviceSetting {
    pub fn is_default(&self) -> bool {
        matches!(self, DeviceSetting::Name(n) if n == "default")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecordingConfig {
    /// "default", a device index (see `wisprcheap devices`), or part of a device name.
    pub device: DeviceSetting,
    /// Recordings shorter than this are discarded.
    pub min_duration_ms: u64,
    /// Keep recording a bit after release so the last word isn't clipped.
    pub tail_ms: u64,
    pub max_duration_sec: f64,
    /// Skip the API calls when the loudest 100 ms of audio is quieter than this (dBFS).
    pub silence_threshold_db: f64,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            device: DeviceSetting::Name("default".into()),
            min_duration_ms: 300,
            tail_ms: 150,
            max_duration_sec: 600.0,
            silence_threshold_db: -55.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Elevenlabs,
    Openai,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Elevenlabs => "elevenlabs",
            Provider::Openai => "openai",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ElevenLabsConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    /// Send the dictionary as keyterms (+$0.05/h surcharge).
    pub keyterms: bool,
    /// Let Scribe drop filler words / false starts itself.
    pub no_verbatim: bool,
}

impl Default for ElevenLabsConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: "https://api.elevenlabs.io".into(),
            model: "scribe_v2".into(),
            keyterms: true,
            no_verbatim: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenAiTranscriptionConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    /// Extra context for the transcriber. Dictionary terms are appended automatically.
    pub prompt: String,
}

impl Default for OpenAiTranscriptionConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-transcribe".into(),
            prompt: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptionConfig {
    pub provider: Provider,
    /// "auto" to detect per recording, or an ISO-639-1 code like "en" / "fr".
    pub language: String,
    pub timeout_ms: u64,
    pub elevenlabs: ElevenLabsConfig,
    pub openai: OpenAiTranscriptionConfig,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Elevenlabs,
            language: "auto".into(),
            timeout_ms: 30_000,
            elevenlabs: ElevenLabsConfig::default(),
            openai: OpenAiTranscriptionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PolishConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    /// Any OpenAI-compatible Chat Completions endpoint (OpenAI, Groq, OpenRouter, Gemini, Ollama...).
    pub base_url: String,
    pub model: String,
    /// Sent as `reasoning_effort`. Set to null for models that don't support it (e.g. gpt-4.1-mini).
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub timeout_ms: u64,
    pub instructions: String,
    /// Skip the polish call when the raw transcript has fewer words than this. 0 = always polish.
    pub min_words: u64,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-6-luna".into(),
            reasoning_effort: Some("none".into()),
            temperature: None,
            timeout_ms: 10_000,
            instructions: DEFAULT_POLISH_INSTRUCTIONS.into(),
            min_words: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DictionaryItem {
    Term(String),
    Full {
        term: String,
        #[serde(default, rename = "soundsLike")]
        sounds_like: Vec<String>,
    },
}

/// LLM used by command mode. Every unset field falls back to the polish settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CommandConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub temperature: Option<Option<f64>>,
    pub timeout_ms: u64,
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: None,
            model: None,
            reasoning_effort: None,
            temperature: None,
            timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairConfig {
    #[serde(default)]
    pub from: Option<String>,
    pub to: String,
}

/// Translation mode: pick a pair in the tray menu and dictations are translated before pasting.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranslationConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub temperature: Option<Option<f64>>,
    pub pairs: Vec<PairConfig>,
    pub timeout_ms: u64,
}

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: None,
            model: None,
            reasoning_effort: None,
            temperature: None,
            pairs: Vec::new(),
            timeout_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Show a notification when something fails (transcription, command, config reload...).
    pub errors: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self { errors: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OutputConfig {
    /// Simulate Ctrl+V into the focused app. If false, the text is only copied to the clipboard.
    pub paste: bool,
    /// Put the previous clipboard content back after pasting.
    pub restore_clipboard: bool,
    /// Append a space so consecutive dictations don't run together.
    pub trailing_space: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            paste: true,
            restore_clipboard: false,
            trailing_space: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SoundsConfig {
    pub enabled: bool,
    pub volume: f64,
}

impl Default for SoundsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 0.25,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// Show a small indicator at the bottom center of the screen while recording and processing.
    pub enabled: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HistoryConfig {
    pub enabled: bool,
    /// Relative to the config file's directory.
    pub path: String,
    /// Keep the audio of recordings whose transcription failed, so nothing is lost.
    pub save_failed_audio: bool,
    pub failed_audio_dir: String,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "history.jsonl".into(),
            save_failed_audio: true,
            failed_audio_dir: "recordings".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub recording: RecordingConfig,
    pub transcription: TranscriptionConfig,
    pub polish: PolishConfig,
    pub dictionary: Option<Vec<DictionaryItem>>,
    pub command: CommandConfig,
    pub translation: TranslationConfig,
    pub notifications: NotificationsConfig,
    pub output: OutputConfig,
    pub sounds: SoundsConfig,
    pub overlay: OverlayConfig,
    pub history: HistoryConfig,
    pub pricing: PricingConfig,
    pub sync: SyncConfig,
}

/// Prices of models the built-in table doesn't know (or that changed). Synced between devices.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PricingConfig {
    pub overrides: Vec<PriceValue>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncHistoryMode {
    /// Upload this device's history entries (statistics readable by the server, text encrypted).
    #[default]
    Upload,
    /// Upload, and also add the other devices' entries to history.jsonl.
    Download,
    /// Keep the history on this device.
    Off,
}

impl SyncHistoryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncHistoryMode::Upload => "upload",
            SyncHistoryMode::Download => "download",
            SyncHistoryMode::Off => "off",
        }
    }
}

/// Optional sync with a self-hosted wisprcheap server (`wisprcheap sync pair` writes this section).
/// Never synced itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncConfig {
    pub server: String,
    /// Device token (`wcs_…`).
    pub token: String,
    /// This device's copy of the data key (`wck_…`), never sent to the server.
    pub key: String,
    pub device_name: String,
    pub history: SyncHistoryMode,
}

impl SyncConfig {
    pub fn enabled(&self) -> bool {
        !self.server.trim().is_empty() && !self.token.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryEntry {
    pub term: String,
    pub sounds_like: Vec<String>,
}

/// Connection settings for an OpenAI-compatible Chat Completions call.
#[derive(Debug, Clone)]
pub struct LlmOptions {
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranslationPair {
    /// Canonical language code of the spoken language, or None to auto-detect.
    pub from: Option<String>,
    pub to: String,
    /// Name of the target language, e.g. "English".
    pub to_name: String,
    /// Stable id used to remember the choice, e.g. "fr>en" or "auto>en".
    pub id: String,
    /// "French → English", "Any → English".
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub base_dir: PathBuf,
    /// The .env files read, in order (later files override earlier ones).
    pub env_files: Vec<PathBuf>,
    pub dictionary: Vec<DictionaryEntry>,
    /// Command-mode LLM with the polish fallbacks applied.
    pub command_llm: LlmOptions,
    /// Translation LLM with the polish fallbacks applied.
    pub translation_llm: LlmOptions,
    pub translation_pairs: Vec<TranslationPair>,
}

// ---------------------------------------------------------------------------
// Environment (.env files)
// ---------------------------------------------------------------------------

/// Values read from .env files. The real environment always wins; these are re-read on each load,
/// so editing .env updates them without a restart.
static ENV_FROM_FILES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn read_env_files(files: &[PathBuf]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for file in files {
        if !file.is_file() {
            continue;
        }
        match dotenvy::from_path_iter(file) {
            Ok(iter) => {
                for (key, value) in iter.flatten() {
                    map.insert(key, value);
                }
            }
            Err(e) => crate::warn!("[config] Could not read {}: {e}", file.display()),
        }
    }
    map
}

/// An environment variable, from the real environment or a loaded .env file.
pub fn env_var(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        return Some(v);
    }
    ENV_FROM_FILES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(name).cloned())
}

/// Replace `${NAME}` occurrences (unset variables become "").
pub fn interpolate(s: &str) -> String {
    interpolate_with(s, &env_var)
}

fn interpolate_with(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let name_len = after
            .char_indices()
            .take_while(|&(i, c)| {
                c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
            })
            .count();
        if name_len > 0 && after[name_len..].starts_with('}') {
            out.push_str(&lookup(&after[..name_len]).unwrap_or_default());
            rest = &after[name_len + 1..];
        } else {
            out.push_str("${");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn interpolate_value(value: &mut serde_yaml::Value, lookup: &dyn Fn(&str) -> Option<String>) {
    match value {
        serde_yaml::Value::String(s) => *s = interpolate_with(s, lookup),
        serde_yaml::Value::Sequence(seq) => seq.iter_mut().for_each(|v| interpolate_value(v, lookup)),
        serde_yaml::Value::Mapping(map) => map
            .iter_mut()
            .for_each(|(_, v)| interpolate_value(v, lookup)),
        serde_yaml::Value::Tagged(t) => interpolate_value(&mut t.value, lookup),
        _ => {}
    }
}

/// A section written as `command:` with everything commented out is null: treat it as empty.
fn drop_null_sections(value: &mut serde_yaml::Value) {
    const SECTIONS: &[&str] = &[
        "hotkey",
        "recording",
        "transcription",
        "polish",
        "command",
        "translation",
        "notifications",
        "output",
        "sounds",
        "overlay",
        "history",
        "pricing",
        "sync",
    ];
    let Some(map) = value.as_mapping_mut() else {
        return;
    };
    for key in SECTIONS {
        if map.get(*key).is_some_and(|v| v.is_null()) {
            map.remove(*key);
        }
    }
    if let Some(t) = map
        .get_mut("transcription")
        .and_then(|v| v.as_mapping_mut())
    {
        for key in ["elevenlabs", "openai"] {
            if t.get(key).is_some_and(|v| v.is_null()) {
                t.remove(key);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Command-line options that affect where the config comes from.
#[derive(Debug, Clone, Default)]
pub struct ConfigArgs {
    pub config: Option<String>,
    /// Read only the `.env` next to the config file, not the one of the app directory (tests).
    pub isolated: bool,
}

pub fn resolve_config_path(args: &ConfigArgs) -> Result<Option<PathBuf>> {
    let candidate = args.config.clone().or_else(|| {
        std::env::var("WISPRCHEAP_CONFIG")
            .ok()
            .filter(|s| !s.is_empty())
    });
    if let Some(candidate) = candidate {
        let p = std::path::absolute(&candidate).unwrap_or_else(|_| PathBuf::from(&candidate));
        if !p.exists() {
            bail!("Config file not found: {}", p.display());
        }
        return Ok(Some(p));
    }
    let default_path = paths().app_dir.join("config.yaml");
    Ok(default_path.is_file().then_some(default_path))
}

/// Directory of the config file in use (or the app directory), without loading it. Never fails.
pub fn resolve_base_dir(args: &ConfigArgs) -> PathBuf {
    match resolve_config_path(args) {
        Ok(Some(p)) => p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| paths().app_dir.clone()),
        _ => paths().app_dir.clone(),
    }
}

fn resolve_llm(
    api_key: &Option<String>,
    base_url: &Option<String>,
    model: &Option<String>,
    reasoning_effort: &Option<Option<String>>,
    temperature: &Option<Option<f64>>,
    polish: &PolishConfig,
    timeout_ms: u64,
) -> LlmOptions {
    LlmOptions {
        api_key: api_key
            .clone()
            .filter(|k| !k.is_empty())
            .or_else(|| polish.api_key.clone()),
        base_url: base_url.clone().unwrap_or_else(|| polish.base_url.clone()),
        model: model.clone().unwrap_or_else(|| polish.model.clone()),
        reasoning_effort: reasoning_effort
            .clone()
            .unwrap_or_else(|| polish.reasoning_effort.clone()),
        temperature: temperature.unwrap_or(polish.temperature),
        timeout_ms,
    }
}

pub fn is_local(url: &str) -> bool {
    url.contains("localhost") || url.contains("127.0.0.1")
}

fn has_key(k: &Option<String>) -> bool {
    k.as_deref().is_some_and(|k| !k.is_empty())
}

/// Range checks the schema types can't express (zod's min/max/positive/...).
fn validate(config: &mut Config) -> Vec<String> {
    let mut issues = Vec::new();
    let mut check = |ok: bool, path: &str, msg: &str| {
        if !ok {
            issues.push(format!("✖ {msg}\n  → at {path}"));
        }
    };
    let h = &config.hotkey;
    check(
        !h.keys.is_empty(),
        "hotkey.keys",
        "Too small: expected array to have >=1 items",
    );
    check(
        h.tap_max_ms > 0,
        "hotkey.tapMaxMs",
        "Too small: expected number to be >0",
    );
    check(
        h.double_tap_window_ms > 0,
        "hotkey.doubleTapWindowMs",
        "Too small: expected number to be >0",
    );
    let r = &config.recording;
    check(
        r.max_duration_sec > 0.0,
        "recording.maxDurationSec",
        "Too small: expected number to be >0",
    );
    check(
        r.silence_threshold_db <= 0.0,
        "recording.silenceThresholdDb",
        "Too big: expected number to be <=0",
    );
    check(
        config.transcription.timeout_ms > 0,
        "transcription.timeoutMs",
        "Too small: expected number to be >0",
    );
    let p = &config.polish;
    check(
        p.timeout_ms > 0,
        "polish.timeoutMs",
        "Too small: expected number to be >0",
    );
    check(
        p.temperature.is_none_or(|t| (0.0..=2.0).contains(&t)),
        "polish.temperature",
        "Expected a number between 0 and 2",
    );
    check(
        !p.instructions.trim().is_empty(),
        "polish.instructions",
        "Too small: expected string to have >=1 characters",
    );
    check(
        config.command.timeout_ms > 0,
        "command.timeoutMs",
        "Too small: expected number to be >0",
    );
    check(
        config
            .command
            .temperature
            .flatten()
            .is_none_or(|t| (0.0..=2.0).contains(&t)),
        "command.temperature",
        "Expected a number between 0 and 2",
    );
    check(
        config.translation.timeout_ms > 0,
        "translation.timeoutMs",
        "Too small: expected number to be >0",
    );
    check(
        config
            .translation
            .temperature
            .flatten()
            .is_none_or(|t| (0.0..=2.0).contains(&t)),
        "translation.temperature",
        "Expected a number between 0 and 2",
    );
    for (i, pair) in config.translation.pairs.iter().enumerate() {
        check(
            pair.to.chars().count() >= 2,
            &format!("translation.pairs[{i}].to"),
            "Too small: expected string to have >=2 characters",
        );
    }
    check(
        (0.0..=1.0).contains(&config.sounds.volume),
        "sounds.volume",
        "Expected a number between 0 and 1",
    );
    if let Some(dict) = &config.dictionary {
        for (i, item) in dict.iter().enumerate() {
            let (term, sounds) = match item {
                DictionaryItem::Term(t) => (t, &[][..]),
                DictionaryItem::Full { term, sounds_like } => (term, &sounds_like[..]),
            };
            check(
                !term.trim().is_empty(),
                &format!("dictionary[{i}]"),
                "Too small: expected string to have >=1 characters",
            );
            for (j, s) in sounds.iter().enumerate() {
                check(
                    !s.trim().is_empty(),
                    &format!("dictionary[{i}].soundsLike[{j}]"),
                    "Too small: expected string to have >=1 characters",
                );
            }
        }
    }
    if let Err(e) = crate::hotkey::validate(&config.hotkey) {
        issues.push(format!("✖ {e}\n  → at hotkey"));
    }
    for (i, o) in config.pricing.overrides.iter().enumerate() {
        let path = format!("pricing.overrides[{i}]");
        if o.model.trim().is_empty() {
            issues.push(format!("✖ Too small: expected string to have >=1 characters\n  → at {path}.model"));
        }
        for (name, v) in [
            ("perMinute", o.per_minute),
            ("inputPerM", o.input_per_m),
            ("outputPerM", o.output_per_m),
        ] {
            if v.is_some_and(|v| !v.is_finite() || v < 0.0) {
                issues.push(format!("✖ Expected a positive number\n  → at {path}.{name}"));
            }
        }
    }
    // zod .trim() transforms.
    config.polish.instructions = config.polish.instructions.trim().to_string();
    issues
}

pub fn load_config(args: &ConfigArgs) -> Result<LoadedConfig> {
    let (loaded, problems) = load_config_lenient(args)?;
    if !problems.is_empty() {
        bail!("Config problems:\n  - {}", problems.join("\n  - "));
    }
    Ok(loaded)
}

/// Like [`load_config`], but missing API keys and bad translation pairs are returned instead of
/// failing (sync needs the config of a device that isn't set up yet). Invalid YAML still fails.
pub fn load_config_lenient(args: &ConfigArgs) -> Result<(LoadedConfig, Vec<String>)> {
    let config_path = resolve_config_path(args)?;
    let base_dir = config_path
        .as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| paths().app_dir.clone());

    let mut env_files = vec![base_dir.join(".env")];
    let app_env = paths().app_dir.join(".env");
    if !args.isolated && !env_files.contains(&app_env) {
        env_files.push(app_env);
    }
    let from_files = read_env_files(&env_files);
    let lookup = |name: &str| std::env::var(name).ok().or_else(|| from_files.get(name).cloned());

    let where_ = config_path
        .as_ref()
        .map(|p| format!(" ({})", p.display()))
        .unwrap_or_default();
    let mut raw: serde_yaml::Value = match &config_path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| anyhow!("Could not read {}: {e}", p.display()))?;
            serde_yaml::from_str(&text).map_err(|e| anyhow!("Invalid config{where_}:\n{e}"))?
        }
        None => serde_yaml::Value::Null,
    };
    if raw.is_null() {
        raw = serde_yaml::Value::Mapping(Default::default());
    }
    interpolate_value(&mut raw, &lookup);
    drop_null_sections(&mut raw);

    let mut config: Config = serde_path_to_error::deserialize(raw).map_err(|e| {
        let path = e.path().to_string();
        let inner = e.into_inner();
        anyhow!("Invalid config{where_}:\n✖ {inner}\n  → at {path}")
    })?;
    let issues = validate(&mut config);
    if !issues.is_empty() {
        bail!("Invalid config{where_}:\n{}", issues.join("\n"));
    }

    // Fall back to the conventional environment variables when keys aren't set in YAML.
    let t = &mut config.transcription;
    if !has_key(&t.elevenlabs.api_key) {
        t.elevenlabs.api_key = lookup("ELEVENLABS_API_KEY");
    }
    if !has_key(&t.openai.api_key) {
        t.openai.api_key = lookup("OPENAI_API_KEY");
    }
    if !has_key(&config.polish.api_key) {
        config.polish.api_key = lookup("OPENAI_API_KEY");
    }
    // Other code reads variables through `env_var`.
    *ENV_FROM_FILES.lock().unwrap() = Some(from_files.clone());

    let mut problems = Vec::new();
    let t = &config.transcription;
    if t.provider == Provider::Elevenlabs && !has_key(&t.elevenlabs.api_key) {
        problems.push(
            "transcription.elevenlabs.apiKey is missing (or set ELEVENLABS_API_KEY in .env)"
                .to_string(),
        );
    }
    if t.provider == Provider::Openai && !has_key(&t.openai.api_key) {
        problems.push(
            "transcription.openai.apiKey is missing (or set OPENAI_API_KEY in .env)".to_string(),
        );
    }
    if config.polish.enabled
        && !has_key(&config.polish.api_key)
        && !is_local(&config.polish.base_url)
    {
        problems.push("polish.apiKey is missing (or set OPENAI_API_KEY in .env), or set polish.enabled: false".to_string());
    }

    let c = &config.command;
    let command_llm = resolve_llm(
        &c.api_key,
        &c.base_url,
        &c.model,
        &c.reasoning_effort,
        &c.temperature,
        &config.polish,
        c.timeout_ms,
    );
    if !config.hotkey.command_keys.is_empty()
        && !has_key(&command_llm.api_key)
        && !is_local(&command_llm.base_url)
    {
        problems.push("command mode needs an API key (command.apiKey, polish.apiKey or OPENAI_API_KEY), or set hotkey.commandKeys: []".to_string());
    }

    let tr = &config.translation;
    let translation_llm = resolve_llm(
        &tr.api_key,
        &tr.base_url,
        &tr.model,
        &tr.reasoning_effort,
        &tr.temperature,
        &config.polish,
        tr.timeout_ms,
    );
    let mut translation_pairs: Vec<TranslationPair> = Vec::new();
    for pair in &tr.pairs {
        let explicit_from = pair
            .from
            .as_deref()
            .filter(|f| !f.is_empty() && !f.eq_ignore_ascii_case("auto"));
        let from = explicit_from.and_then(canonical_language);
        let to = canonical_language(&pair.to);
        let (Some(to), true) = (to, explicit_from.is_none() || from.is_some()) else {
            problems.push(format!(
                "translation.pairs: \"{} -> {}\" uses an invalid language code",
                pair.from.as_deref().unwrap_or("auto"),
                pair.to
            ));
            continue;
        };
        let id = format!("{}>{}", from.as_deref().unwrap_or("auto"), to);
        if translation_pairs.iter().any(|p| p.id == id) {
            continue;
        }
        let label = format!(
            "{} → {}",
            from.as_deref()
                .map(language_name)
                .unwrap_or_else(|| "Any".to_string()),
            language_name(&to)
        );
        translation_pairs.push(TranslationPair {
            to_name: language_name(&to),
            from,
            to,
            id,
            label,
        });
    }
    if !translation_pairs.is_empty()
        && !has_key(&translation_llm.api_key)
        && !is_local(&translation_llm.base_url)
    {
        problems.push(
            "translation needs an API key (translation.apiKey, polish.apiKey or OPENAI_API_KEY)"
                .to_string(),
        );
    }

    let mut seen = HashSet::new();
    let mut dictionary = Vec::new();
    for item in config.dictionary.iter().flatten() {
        let entry = match item {
            DictionaryItem::Term(t) => DictionaryEntry {
                term: t.trim().to_string(),
                sounds_like: Vec::new(),
            },
            DictionaryItem::Full { term, sounds_like } => DictionaryEntry {
                term: term.trim().to_string(),
                sounds_like: sounds_like.iter().map(|s| s.trim().to_string()).collect(),
            },
        };
        if seen.insert(entry.term.to_lowercase()) {
            dictionary.push(entry);
        }
    }

    Ok((
        LoadedConfig {
            config,
            config_path,
            base_dir,
            env_files,
            dictionary,
            command_llm,
            translation_llm,
            translation_pairs,
        },
        problems,
    ))
}

/// The config file to create/edit: the one in use, or the app's config.yaml (created from the example).
pub fn ensure_config_file(config_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = config_path {
        return Ok(p.to_path_buf());
    }
    let file = paths().app_dir.join("config.yaml");
    if !file.exists() {
        std::fs::create_dir_all(&paths().app_dir)?;
        std::fs::write(&file, EXAMPLE_CONFIG)?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_str(yaml: &str) -> Result<LoadedConfig> {
        let dir = std::env::temp_dir().join(format!(
            "wisprcheap-test-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.yaml");
        std::fs::write(&file, yaml).unwrap();
        let result = load_config(&ConfigArgs {
            config: Some(file.to_string_lossy().into_owned()),
            isolated: true,
        });
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    const KEYS: &str = "transcription:\n  elevenlabs:\n    apiKey: x\npolish:\n  apiKey: y\n";

    #[test]
    fn defaults_apply() {
        let loaded = load_str(KEYS).unwrap();
        assert_eq!(loaded.config.hotkey.keys, vec!["Ctrl", "Win"]);
        assert_eq!(loaded.config.polish.model, "gpt-6-luna");
        assert_eq!(
            loaded.config.polish.reasoning_effort.as_deref(),
            Some("none")
        );
        assert_eq!(loaded.command_llm.model, "gpt-6-luna");
        assert_eq!(loaded.command_llm.api_key.as_deref(), Some("y"));
        assert_eq!(
            loaded.config.recording.device,
            DeviceSetting::Name("default".into())
        );
    }

    #[test]
    fn invalid_provider_mentions_path() {
        let err = load_str("transcription:\n  provider: nope\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("transcription.provider"), "{err}");
    }

    #[test]
    fn explicit_null_disables_reasoning_effort_for_command() {
        let loaded = load_str(&format!("{KEYS}command:\n  reasoningEffort: null\n")).unwrap();
        assert_eq!(loaded.command_llm.reasoning_effort, None);
        let loaded = load_str(KEYS).unwrap();
        assert_eq!(loaded.command_llm.reasoning_effort.as_deref(), Some("none"));
    }

    #[test]
    fn translation_pairs() {
        let loaded = load_str(&format!(
            "{KEYS}translation:\n  pairs:\n    - {{ from: fra, to: EN }}\n    - {{ to: fr }}\n    - {{ from: fr, to: en }}\n"
        ))
        .unwrap();
        let ids: Vec<_> = loaded
            .translation_pairs
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, vec!["fr>en", "auto>fr"]);
        assert_eq!(loaded.translation_pairs[0].label, "French → English");
        assert_eq!(loaded.translation_pairs[1].label, "Any → French");
        let err = load_str(&format!(
            "{KEYS}translation:\n  pairs:\n    - {{ to: not a language }}\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid language code"), "{err}");
    }

    #[test]
    fn dictionary_normalized_and_deduplicated() {
        let loaded = load_str(&format!(
            "{KEYS}dictionary:\n  - Kubernetes\n  - term: pnpm\n    soundsLike: [p n p m]\n  - kubernetes\n"
        ))
        .unwrap();
        assert_eq!(loaded.dictionary.len(), 2);
        assert_eq!(loaded.dictionary[1].sounds_like, vec!["p n p m"]);
    }

    #[test]
    fn interpolates_env() {
        assert_eq!(interpolate("a ${WISPRCHEAP_SURELY_UNSET_VAR} b"), "a  b");
        assert_eq!(interpolate("keep ${ not} $x"), "keep ${ not} $x");
    }

    #[test]
    fn sync_and_pricing_sections() {
        let loaded = load_str(&format!(
            "{KEYS}sync:\n  server: https://s\n  token: wcs_x\n  history: download\npricing:\n  overrides:\n    - {{ model: m, perMinute: 0.01 }}\n"
        ))
        .unwrap();
        assert!(loaded.config.sync.enabled());
        assert_eq!(loaded.config.sync.history, SyncHistoryMode::Download);
        assert_eq!(loaded.config.pricing.overrides[0].per_minute, Some(0.01));
        assert!(!load_str(KEYS).unwrap().config.sync.enabled());
        let err = load_str(&format!("{KEYS}pricing:\n  overrides:\n    - {{ model: m, perMinute: -1 }}\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("pricing.overrides[0].perMinute"), "{err}");
        let err = load_str(&format!("{KEYS}sync:\n  history: sometimes\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sync.history"), "{err}");
    }

    #[test]
    fn missing_keys_are_reported_together() {
        let err = load_str("polish:\n  enabled: true\n  apiKey: ''\ntranscription:\n  elevenlabs:\n    apiKey: ''\n")
            .map(|_| ())
            .unwrap_err()
            .to_string();
        // The real environment may define the keys; only assert when it doesn't.
        if std::env::var("ELEVENLABS_API_KEY").is_err() {
            assert!(
                err.contains("transcription.elevenlabs.apiKey is missing"),
                "{err}"
            );
        }
    }
}
