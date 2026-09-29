//! The synced profile of this device (SPEC.md section 5) read from the effective config, and remote
//! changes written back into `config.yaml` / `.env` (the files stay the source of truth).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use wisprcheap_sync::crypto::DataKey;
use wisprcheap_sync::profile::{
    self, DictValue, KIND_DICT, KIND_PAIR, KIND_PRICE, KIND_SECRET, KIND_SETTING, PairValue,
    PriceValue, SETTINGS, dict_key, is_inherit, pair_key, price_key, setting_def, value_matches,
};

use crate::config::{Config, LoadedConfig};
use crate::env_edit;
use crate::lang::canonical_language;
use crate::sync::state::record_key;
use crate::yaml_edit::{self, ItemStyle, YamlText};

/// One record of the local profile.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRecord {
    pub kind: &'static str,
    pub id: String,
    pub value: Value,
    /// Position in the config (items are sent in this order, so lists keep it on other devices).
    pub order: usize,
}

/// `kind/id` → record.
pub type Profile = BTreeMap<String, LocalRecord>;

fn inherit_or<T: serde::Serialize>(v: &Option<T>) -> Value {
    match v {
        Some(v) => json!(v),
        None => profile::inherit(),
    }
}

/// Command / translation options: key absent = inherit, `null` = omit, else the value.
fn tri<T: serde::Serialize>(v: &Option<Option<T>>) -> Value {
    match v {
        None => profile::inherit(),
        Some(v) => json!(v),
    }
}

fn setting_value(c: &Config, id: &str) -> Option<Value> {
    let t = &c.transcription;
    let p = &c.polish;
    let cmd = &c.command;
    let tr = &c.translation;
    Some(match id {
        "transcription.provider" => json!(t.provider.as_str()),
        "transcription.language" => json!(t.language),
        "transcription.timeoutMs" => json!(t.timeout_ms),
        "transcription.elevenlabs.model" => json!(t.elevenlabs.model),
        "transcription.elevenlabs.baseUrl" => json!(t.elevenlabs.base_url),
        "transcription.elevenlabs.keyterms" => json!(t.elevenlabs.keyterms),
        "transcription.elevenlabs.noVerbatim" => json!(t.elevenlabs.no_verbatim),
        "transcription.openai.model" => json!(t.openai.model),
        "transcription.openai.baseUrl" => json!(t.openai.base_url),
        "transcription.openai.prompt" => json!(t.openai.prompt),
        "polish.enabled" => json!(p.enabled),
        "polish.baseUrl" => json!(p.base_url),
        "polish.model" => json!(p.model),
        "polish.reasoningEffort" => json!(p.reasoning_effort),
        "polish.temperature" => json!(p.temperature),
        "polish.timeoutMs" => json!(p.timeout_ms),
        "polish.minWords" => json!(p.min_words),
        "polish.instructions" => json!(p.instructions),
        "command.baseUrl" => inherit_or(&cmd.base_url),
        "command.model" => inherit_or(&cmd.model),
        "command.reasoningEffort" => tri(&cmd.reasoning_effort),
        "command.temperature" => tri(&cmd.temperature),
        "command.timeoutMs" => json!(cmd.timeout_ms),
        "translation.baseUrl" => inherit_or(&tr.base_url),
        "translation.model" => inherit_or(&tr.model),
        "translation.reasoningEffort" => tri(&tr.reasoning_effort),
        "translation.temperature" => tri(&tr.temperature),
        "translation.timeoutMs" => json!(tr.timeout_ms),
        _ => return None,
    })
}

fn key_of(k: &Option<String>) -> String {
    k.as_deref().map(str::trim).unwrap_or_default().to_string()
}

/// Synced API keys from the effective config; `""` = the fallback (SPEC.md 5.3).
fn secret_values(c: &Config) -> [(&'static str, String); 5] {
    let elevenlabs = key_of(&c.transcription.elevenlabs.api_key);
    let openai = key_of(&c.transcription.openai.api_key);
    let polish_effective = key_of(&c.polish.api_key);
    let polish = if polish_effective == openai {
        String::new()
    } else {
        polish_effective.clone()
    };
    let secondary = |k: &Option<String>| {
        let k = key_of(k);
        if k == polish_effective { String::new() } else { k }
    };
    [
        ("elevenlabs", elevenlabs),
        ("openai", openai),
        ("polish", polish),
        ("command", secondary(&c.command.api_key)),
        ("translation", secondary(&c.translation.api_key)),
    ]
}

/// The canonical profile of this device.
pub fn profile(loaded: &LoadedConfig, dk: &DataKey) -> Profile {
    let c = &loaded.config;
    let mut out = Profile::new();
    let mut put = |kind: &'static str, id: String, value: Value| {
        let order = out.len();
        out.insert(record_key(kind, &id), LocalRecord { kind, id, value, order });
    };
    for def in SETTINGS {
        if let Some(v) = setting_value(c, def.id) {
            put(KIND_SETTING, def.id.to_string(), v);
        }
    }
    for (name, value) in secret_values(c) {
        put(KIND_SECRET, name.to_string(), json!(value));
    }
    for entry in &loaded.dictionary {
        let v = DictValue {
            term: entry.term.clone(),
            sounds_like: entry.sounds_like.clone(),
        };
        put(
            KIND_DICT,
            dk.blind_id(KIND_DICT, &dict_key(&entry.term)),
            serde_json::to_value(v).expect("json"),
        );
    }
    for pair in &loaded.translation_pairs {
        let v = PairValue {
            from: pair.from.clone(),
            to: pair.to.clone(),
        };
        put(
            KIND_PAIR,
            dk.blind_id(KIND_PAIR, &pair_key(pair.from.as_deref(), &pair.to)),
            serde_json::to_value(v).expect("json"),
        );
    }
    let mut seen = HashSet::new();
    for o in &c.pricing.overrides {
        let model = price_key(&o.model);
        if model.is_empty() || !seen.insert(model.clone()) {
            continue;
        }
        let v = PriceValue {
            model: model.clone(),
            ..o.clone()
        };
        put(
            KIND_PRICE,
            dk.blind_id(KIND_PRICE, &model),
            serde_json::to_value(v).expect("json"),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Write-back
// ---------------------------------------------------------------------------

/// A remote record to write locally (`value` None = deleted).
#[derive(Debug, Clone)]
pub struct Incoming {
    pub kind: String,
    pub id: String,
    pub value: Option<Value>,
}

impl Incoming {
    pub fn key(&self) -> String {
        record_key(&self.kind, &self.id)
    }
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    /// Records that couldn't be written (key, reason): they stay pending.
    pub failed: Vec<(String, String)>,
    /// Records ignored because their value is invalid here (key, reason).
    pub ignored: Vec<(String, String)>,
    /// Local changes made, per kind.
    pub changed: BTreeMap<String, usize>,
    pub files_changed: bool,
}

fn to_yaml(v: &Value) -> Result<serde_yaml::Value> {
    Ok(serde_yaml::to_value(v)?)
}

/// Values the desktop config would refuse (a reload would fail).
fn setting_problem(id: &str, v: &Value) -> Option<String> {
    let bad = |why: &str| Some(format!("{id}: {why}"));
    if is_inherit(v) || v.is_null() {
        return None;
    }
    if id == "transcription.provider" && !matches!(v.as_str(), Some("elevenlabs" | "openai")) {
        return bad("unknown provider");
    }
    if id.ends_with(".timeoutMs") && v.as_u64().is_none_or(|n| n == 0) {
        return bad("must be a positive number");
    }
    if id.ends_with(".temperature") && v.as_f64().is_none_or(|t| !(0.0..=2.0).contains(&t)) {
        return bad("must be between 0 and 2");
    }
    if id == "polish.minWords" && v.as_u64().is_none() {
        return bad("must be a positive number");
    }
    if (id == "polish.instructions" || id.ends_with(".model") || id.ends_with(".baseUrl"))
        && v.as_str().is_none_or(|s| s.trim().is_empty())
    {
        return bad("must not be empty");
    }
    None
}

fn raw_dict_key(item: &serde_yaml::Value) -> Option<String> {
    let term = item
        .as_str()
        .or_else(|| item.get("term").and_then(serde_yaml::Value::as_str))?;
    Some(dict_key(term))
}

fn raw_dict_value(item: &serde_yaml::Value) -> Option<DictValue> {
    if let Some(t) = item.as_str() {
        return Some(DictValue {
            term: t.trim().into(),
            sounds_like: vec![],
        });
    }
    let term = item.get("term")?.as_str()?.trim().to_string();
    let sounds_like = item
        .get("soundsLike")
        .and_then(serde_yaml::Value::as_sequence)
        .map(|s| {
            s.iter()
                .filter_map(|x| x.as_str().map(|x| x.trim().to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(DictValue { term, sounds_like })
}

fn dict_item(v: &DictValue) -> serde_yaml::Value {
    if v.sounds_like.is_empty() {
        return serde_yaml::Value::String(v.term.clone());
    }
    let mut m = serde_yaml::Mapping::new();
    m.insert("term".into(), v.term.clone().into());
    m.insert(
        "soundsLike".into(),
        serde_yaml::Value::Sequence(v.sounds_like.iter().map(|s| s.clone().into()).collect()),
    );
    serde_yaml::Value::Mapping(m)
}

/// Canonical (from, to) of a `translation.pairs` item.
fn raw_pair(item: &serde_yaml::Value) -> Option<(Option<String>, String)> {
    let to = canonical_language(item.get("to")?.as_str()?)?;
    let from = match item.get("from").and_then(serde_yaml::Value::as_str) {
        Some(f) if !f.trim().is_empty() && !f.eq_ignore_ascii_case("auto") => {
            Some(canonical_language(f)?)
        }
        _ => None,
    };
    Some((from, to))
}

fn pair_item(v: &PairValue) -> serde_yaml::Value {
    let mut m = serde_yaml::Mapping::new();
    if let Some(f) = &v.from {
        m.insert("from".into(), f.clone().into());
    }
    m.insert("to".into(), v.to.clone().into());
    serde_yaml::Value::Mapping(m)
}

fn raw_price(item: &serde_yaml::Value) -> Option<PriceValue> {
    let mut v: PriceValue = serde_yaml::from_value(item.clone()).ok()?;
    v.model = price_key(&v.model);
    Some(v)
}

fn price_item(v: &PriceValue) -> Result<serde_yaml::Value> {
    Ok(serde_yaml::to_value(v)?)
}

struct SecretSpec {
    name: &'static str,
    path: &'static [&'static str],
    /// Variable read when the YAML value is empty.
    implicit: Option<&'static str>,
    /// Variable used when the key needs a variable of its own.
    dedicated: &'static str,
}

const SECRET_SPECS: [SecretSpec; 5] = [
    SecretSpec {
        name: "elevenlabs",
        path: &["transcription", "elevenlabs", "apiKey"],
        implicit: Some("ELEVENLABS_API_KEY"),
        dedicated: "WISPRCHEAP_ELEVENLABS_API_KEY",
    },
    SecretSpec {
        name: "openai",
        path: &["transcription", "openai", "apiKey"],
        implicit: Some("OPENAI_API_KEY"),
        dedicated: "WISPRCHEAP_OPENAI_API_KEY",
    },
    SecretSpec {
        name: "polish",
        path: &["polish", "apiKey"],
        implicit: Some("OPENAI_API_KEY"),
        dedicated: "WISPRCHEAP_POLISH_API_KEY",
    },
    SecretSpec {
        name: "command",
        path: &["command", "apiKey"],
        implicit: None,
        dedicated: "WISPRCHEAP_COMMAND_API_KEY",
    },
    SecretSpec {
        name: "translation",
        path: &["translation", "apiKey"],
        implicit: None,
        dedicated: "WISPRCHEAP_TRANSLATION_API_KEY",
    },
];

fn spec(name: &str) -> &'static SecretSpec {
    SECRET_SPECS
        .iter()
        .find(|s| s.name == name)
        .expect("known secret")
}

/// Where a key comes from.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// Written in the YAML.
    Literal,
    /// `${VAR}` in the YAML, or the conventional variable when the YAML has nothing.
    Env(String),
    /// Nothing: same key as cleanup (command / translation).
    Inherit,
}

fn single_reference(s: &str) -> Option<String> {
    let name = s.trim().strip_prefix("${")?.strip_suffix('}')?;
    let valid = !name.is_empty()
        && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
        && !name.starts_with(|c: char| c.is_ascii_digit());
    valid.then(|| name.to_string())
}

fn source(doc: &serde_yaml::Value, s: &SecretSpec) -> Source {
    match yaml_edit::get(doc, s.path).and_then(serde_yaml::Value::as_str) {
        Some(v) if !v.trim().is_empty() => match single_reference(v) {
            Some(var) => Source::Env(var),
            None => Source::Literal,
        },
        _ => match s.implicit {
            Some(v) => Source::Env(v.into()),
            None => Source::Inherit,
        },
    }
}

/// The variable a secret is read from, following command/translation → cleanup.
fn variable_of(doc: &serde_yaml::Value, name: &str) -> Option<String> {
    match source(doc, spec(name)) {
        Source::Env(v) => Some(v),
        Source::Inherit => variable_of(doc, "polish"),
        Source::Literal => None,
    }
}

/// Effective key of each secret once `targets` (`""` = fallback) are in place.
fn effective(targets: &BTreeMap<&'static str, String>) -> BTreeMap<&'static str, String> {
    let t = |n: &str| targets.get(n).cloned().unwrap_or_default();
    let mut e = BTreeMap::new();
    e.insert("elevenlabs", t("elevenlabs"));
    e.insert("openai", t("openai"));
    let polish = if t("polish").is_empty() { t("openai") } else { t("polish") };
    e.insert("polish", polish.clone());
    for n in ["command", "translation"] {
        e.insert(n, if t(n).is_empty() { polish.clone() } else { t(n) });
    }
    e
}

/// Writes remote records into the config file and `.env`. `backup`: keep a copy of the config
/// first (first sync of a device).
pub fn apply(
    loaded: &LoadedConfig,
    dk: &DataKey,
    incoming: &[Incoming],
    backup: bool,
) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    let config_file = loaded
        .config_path
        .clone()
        .ok_or_else(|| anyhow!("no config file to write to"))?;
    let original = std::fs::read_to_string(&config_file)
        .map_err(|e| anyhow!("can't read {}: {e}", config_file.display()))?;
    let mut y = YamlText::new(original.clone());
    let local = profile(loaded, dk);
    let changed = |report: &mut ApplyReport, kind: &str| {
        *report.changed.entry(kind.to_string()).or_default() += 1;
    };

    let mut secrets: Vec<(&Incoming, String)> = Vec::new();
    for inc in incoming {
        let key = inc.key();
        let unchanged = match (&inc.value, local.get(&key)) {
            (Some(v), Some(l)) => &l.value == v,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            continue;
        }
        let result: Result<bool> = match inc.kind.as_str() {
            KIND_SETTING => apply_setting(&mut y, inc, &mut report),
            KIND_DICT => apply_dict(&mut y, dk, inc),
            KIND_PAIR => apply_pair(&mut y, dk, inc),
            KIND_PRICE => apply_price(&mut y, dk, inc),
            KIND_SECRET => {
                match inc.value.as_ref().and_then(Value::as_str) {
                    Some(s) if profile::SECRETS.contains(&inc.id.as_str()) => {
                        secrets.push((inc, s.to_string()))
                    }
                    _ => report
                        .ignored
                        .push((key.clone(), "not a known API key".into())),
                }
                continue;
            }
            _ => Ok(false),
        };
        match result {
            Ok(true) => changed(&mut report, &inc.kind),
            Ok(false) => {}
            Err(e) => report.failed.push((key, e.to_string())),
        }
    }

    // API keys, in dependency order (cleanup falls back to OpenAI, command/translation to cleanup).
    let mut env_writes: Vec<(String, String)> = Vec::new();
    if !secrets.is_empty() {
        let mut targets: BTreeMap<&'static str, String> = secret_values(&loaded.config)
            .into_iter()
            .collect();
        for (inc, v) in &secrets {
            targets.insert(spec(&inc.id).name, v.clone());
        }
        let eff = effective(&targets);
        secrets.sort_by_key(|(inc, _)| SECRET_SPECS.iter().position(|s| s.name == inc.id));
        for (inc, v) in &secrets {
            match apply_secret(&mut y, spec(&inc.id), v, &eff, &mut env_writes) {
                Ok(()) => changed(&mut report, KIND_SECRET),
                Err(e) => report.failed.push((inc.key(), e.to_string())),
            }
        }
    }

    // .env first: the config may reference the new variables.
    for (var, value) in &env_writes {
        let file = env_file_for(loaded, var);
        env_edit::set_var(&file, var, value)?;
        report.files_changed = true;
    }
    if y.as_str() != original {
        if backup {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let name = config_file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "config.yaml".into());
            let copy = config_file.with_file_name(format!("{name}.bak-{stamp}"));
            std::fs::write(&copy, &original)
                .map_err(|e| anyhow!("can't back up the config to {}: {e}", copy.display()))?;
        }
        std::fs::write(&config_file, y.as_str())
            .map_err(|e| anyhow!("can't write {}: {e}", config_file.display()))?;
        report.files_changed = true;
    }
    Ok(report)
}

/// The `.env` file where `var` should be written: the last one defining it, else the one next to
/// the config.
fn env_file_for(loaded: &LoadedConfig, var: &str) -> PathBuf {
    loaded
        .env_files
        .iter()
        .rev()
        .find(|f| env_edit::defines(f, var))
        .cloned()
        .unwrap_or_else(|| loaded.base_dir.join(".env"))
}

fn apply_setting(y: &mut YamlText, inc: &Incoming, report: &mut ApplyReport) -> Result<bool> {
    let key = inc.key();
    let Some(def) = setting_def(&inc.id) else {
        // A setting from a newer app version: nothing to do here.
        return Ok(false);
    };
    let Some(v) = &inc.value else {
        return Ok(false);
    };
    if !value_matches(def.ty, v) {
        report.ignored.push((key, "unexpected value type".into()));
        return Ok(false);
    }
    if let Some(problem) = setting_problem(&inc.id, v) {
        report.ignored.push((key, problem));
        return Ok(false);
    }
    let path: Vec<&str> = inc.id.split('.').collect();
    if is_inherit(v) {
        y.remove(&path)?;
    } else {
        y.set(&path, &to_yaml(v)?)?;
    }
    Ok(true)
}

fn apply_dict(y: &mut YamlText, dk: &DataKey, inc: &Incoming) -> Result<bool> {
    let path = ["dictionary"];
    match &inc.value {
        Some(v) => {
            let v: DictValue = serde_json::from_value(v.clone())?;
            let key = dict_key(&v.term);
            if key.is_empty() || dk.blind_id(KIND_DICT, &key) != inc.id {
                return Ok(false);
            }
            let doc = y.doc()?;
            let existing = yaml_edit::get(&doc, &path)
                .and_then(serde_yaml::Value::as_sequence)
                .and_then(|s| s.iter().find(|i| raw_dict_key(i).as_deref() == Some(&key)))
                .and_then(raw_dict_value);
            if existing.as_ref() == Some(&v) {
                return Ok(false);
            }
            y.list_upsert(
                &path,
                &|i| raw_dict_key(i).as_deref() == Some(key.as_str()),
                dict_item(&v),
                ItemStyle::Block,
            )?;
        }
        None => {
            y.list_remove(&path, &|i| {
                raw_dict_key(i).is_some_and(|k| dk.blind_id(KIND_DICT, &k) == inc.id)
            })?;
        }
    }
    Ok(true)
}

fn apply_pair(y: &mut YamlText, dk: &DataKey, inc: &Incoming) -> Result<bool> {
    let path = ["translation", "pairs"];
    let blind = |p: &(Option<String>, String)| dk.blind_id(KIND_PAIR, &pair_key(p.0.as_deref(), &p.1));
    match &inc.value {
        Some(v) => {
            let v: PairValue = serde_json::from_value(v.clone())?;
            let Some(to) = canonical_language(&v.to) else {
                return Ok(false);
            };
            let from = match &v.from {
                Some(f) => Some(canonical_language(f).ok_or_else(|| anyhow!("invalid language {f}"))?),
                None => None,
            };
            let canon = (from, to);
            if blind(&canon) != inc.id {
                return Ok(false);
            }
            let doc = y.doc()?;
            let exists = yaml_edit::get(&doc, &path)
                .and_then(serde_yaml::Value::as_sequence)
                .is_some_and(|s| s.iter().any(|i| raw_pair(i).as_ref() == Some(&canon)));
            if exists {
                return Ok(false);
            }
            let item = pair_item(&PairValue {
                from: canon.0.clone(),
                to: canon.1.clone(),
            });
            y.list_upsert(&path, &|i| raw_pair(i).as_ref() == Some(&canon), item, ItemStyle::Flow)?;
        }
        None => {
            y.list_remove(&path, &|i| raw_pair(i).is_some_and(|p| blind(&p) == inc.id))?;
        }
    }
    Ok(true)
}

fn apply_price(y: &mut YamlText, dk: &DataKey, inc: &Incoming) -> Result<bool> {
    let path = ["pricing", "overrides"];
    match &inc.value {
        Some(v) => {
            let mut v: PriceValue = serde_json::from_value(v.clone())?;
            v.model = price_key(&v.model);
            if v.model.is_empty() || dk.blind_id(KIND_PRICE, &v.model) != inc.id {
                return Ok(false);
            }
            let bad = [v.per_minute, v.input_per_m, v.output_per_m]
                .into_iter()
                .flatten()
                .any(|p| !p.is_finite() || p < 0.0);
            if bad {
                return Ok(false);
            }
            let model = v.model.clone();
            y.list_upsert(
                &path,
                &|i| raw_price(i).is_some_and(|p| p.model == model),
                price_item(&v)?,
                ItemStyle::Flow,
            )?;
        }
        None => {
            y.list_remove(&path, &|i| {
                raw_price(i).is_some_and(|p| dk.blind_id(KIND_PRICE, &p.model) == inc.id)
            })?;
        }
    }
    Ok(true)
}

fn apply_secret(
    y: &mut YamlText,
    s: &SecretSpec,
    value: &str,
    eff: &BTreeMap<&'static str, String>,
    env_writes: &mut Vec<(String, String)>,
) -> Result<()> {
    let doc = y.doc()?;
    let path = s.path;
    // Fallbacks.
    if value.is_empty() {
        match s.name {
            "polish" => {
                // Same key as the OpenAI transcription.
                let openai = yaml_edit::get(&doc, spec("openai").path)
                    .and_then(serde_yaml::Value::as_str)
                    .filter(|v| !v.trim().is_empty())
                    .map(String::from);
                return match openai {
                    Some(raw) => y.set(path, &serde_yaml::Value::String(raw)),
                    None => y.remove(path),
                };
            }
            "command" | "translation" => return y.remove(path),
            _ => {}
        }
    }
    let src = source(&doc, s);
    if src == Source::Literal {
        return y.set(path, &serde_yaml::Value::String(value.into()));
    }
    // Is the variable ours alone? (Another key reading it wants a different value, or the real
    // environment overrides the .env files.)
    let usable = match &src {
        Source::Env(var) => {
            let shared = SECRET_SPECS.iter().any(|other| {
                other.name != s.name
                    && variable_of(&doc, other.name).as_deref() == Some(var.as_str())
                    && eff.get(other.name).map(String::as_str) != Some(value)
            });
            (!shared && std::env::var(var).is_err()).then(|| var.clone())
        }
        _ => None,
    };
    match usable {
        Some(var) => env_writes.push((var, value.to_string())),
        None => {
            let var = s.dedicated.to_string();
            if std::env::var(&var).is_ok() {
                return Err(anyhow!("{var} is set in the environment; can't store the new key"));
            }
            env_writes.push((var.clone(), value.to_string()));
            y.set(path, &serde_yaml::Value::String(format!("${{{var}}}")))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_fall_back() {
        let mut c = Config::default();
        c.transcription.openai.api_key = Some("sk-a".into());
        c.polish.api_key = Some("sk-a".into());
        c.command.api_key = Some("sk-a".into());
        c.translation.api_key = Some("gsk".into());
        let s: BTreeMap<_, _> = secret_values(&c).into_iter().collect();
        assert_eq!(s["openai"], "sk-a");
        assert_eq!(s["polish"], "");
        assert_eq!(s["command"], "");
        assert_eq!(s["translation"], "gsk");
        assert_eq!(s["elevenlabs"], "");
    }

    #[test]
    fn tri_state_options() {
        let mut c = Config::default();
        assert_eq!(setting_value(&c, "command.temperature"), Some(profile::inherit()));
        c.command.temperature = Some(None);
        assert_eq!(setting_value(&c, "command.temperature"), Some(Value::Null));
        c.command.temperature = Some(Some(0.5));
        assert_eq!(setting_value(&c, "command.temperature"), Some(json!(0.5)));
        for def in SETTINGS {
            let v = setting_value(&c, def.id).unwrap_or_else(|| panic!("{}", def.id));
            assert!(value_matches(def.ty, &v), "{}: {v}", def.id);
        }
    }

    #[test]
    fn references() {
        assert_eq!(single_reference("${OPENAI_API_KEY}").as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(single_reference("Bearer ${X}"), None);
        let doc: serde_yaml::Value = serde_yaml::from_str(
            "transcription:\n  openai:\n    apiKey: ${OPENAI_API_KEY}\npolish:\n  apiKey: literal\n",
        )
        .unwrap();
        assert_eq!(variable_of(&doc, "openai").as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(variable_of(&doc, "elevenlabs").as_deref(), Some("ELEVENLABS_API_KEY"));
        assert_eq!(variable_of(&doc, "command"), None);
        assert_eq!(source(&doc, spec("polish")), Source::Literal);
    }
}
