//! Canonical synced profile (SPEC.md section 5): record kinds, setting ids and value shapes, shared by
//! every client so that a setting means the same thing on the desktop and on Android.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const KIND_SETTING: &str = "setting";
pub const KIND_SECRET: &str = "secret";
pub const KIND_DICT: &str = "dict";
pub const KIND_PAIR: &str = "pair";
pub const KIND_PRICE: &str = "price";
pub const KIND_HISTORY: &str = "history";
pub const KINDS: [&str; 6] = [KIND_SETTING, KIND_SECRET, KIND_DICT, KIND_PAIR, KIND_PRICE, KIND_HISTORY];

/// Shape of a setting value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Str,
    Int,
    Bool,
    /// String, or `null` (the option is omitted from requests).
    NullableStr,
    /// Number, or `null`.
    NullableNum,
    /// String, or `{"inherit": true}` (same as the cleanup setting).
    InheritStr,
    /// String, `null` or `{"inherit": true}`.
    InheritNullableStr,
    /// Number, `null` or `{"inherit": true}`.
    InheritNullableNum,
}

pub struct SettingDef {
    pub id: &'static str,
    pub ty: ValueType,
}

const fn s(id: &'static str, ty: ValueType) -> SettingDef {
    SettingDef { id, ty }
}

use ValueType::*;

/// Every synced setting. Anything else is per device.
pub const SETTINGS: &[SettingDef] = &[
    s("transcription.provider", Str),
    s("transcription.language", Str),
    s("transcription.timeoutMs", Int),
    s("transcription.elevenlabs.model", Str),
    s("transcription.elevenlabs.baseUrl", Str),
    s("transcription.elevenlabs.keyterms", Bool),
    s("transcription.elevenlabs.noVerbatim", Bool),
    s("transcription.openai.model", Str),
    s("transcription.openai.baseUrl", Str),
    s("transcription.openai.prompt", Str),
    s("polish.enabled", Bool),
    s("polish.baseUrl", Str),
    s("polish.model", Str),
    s("polish.reasoningEffort", NullableStr),
    s("polish.temperature", NullableNum),
    s("polish.timeoutMs", Int),
    s("polish.minWords", Int),
    s("polish.instructions", Str),
    s("command.baseUrl", InheritStr),
    s("command.model", InheritStr),
    s("command.reasoningEffort", InheritNullableStr),
    s("command.temperature", InheritNullableNum),
    s("command.timeoutMs", Int),
    s("translation.baseUrl", InheritStr),
    s("translation.model", InheritStr),
    s("translation.reasoningEffort", InheritNullableStr),
    s("translation.temperature", InheritNullableNum),
    s("translation.timeoutMs", Int),
];

/// Synced API keys. `""` for polish/command/translation means "fallback" (SPEC.md 5.3).
pub const SECRETS: [&str; 5] = ["elevenlabs", "openai", "polish", "command", "translation"];

pub fn setting_def(id: &str) -> Option<&'static SettingDef> {
    SETTINGS.iter().find(|d| d.id == id)
}

pub fn inherit() -> Value {
    serde_json::json!({ "inherit": true })
}

pub fn is_inherit(v: &Value) -> bool {
    v.get("inherit").and_then(Value::as_bool) == Some(true)
}

/// Whether `value` has the shape declared for `ty`.
pub fn value_matches(ty: ValueType, value: &Value) -> bool {
    match ty {
        Str => value.is_string(),
        Int => value.is_i64() || value.is_u64(),
        Bool => value.is_boolean(),
        NullableStr => value.is_string() || value.is_null(),
        NullableNum => value.is_number() || value.is_null(),
        InheritStr => value.is_string() || is_inherit(value),
        InheritNullableStr => value.is_string() || value.is_null() || is_inherit(value),
        InheritNullableNum => value.is_number() || value.is_null() || is_inherit(value),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictValue {
    pub term: String,
    #[serde(default)]
    pub sounds_like: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairValue {
    /// Spoken language code, or null for auto-detect.
    pub from: Option<String>,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceValue {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_minute: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_m: Option<f64>,
}

/// Key blinded into a dictionary record id: the trimmed, lowercase term.
pub fn dict_key(term: &str) -> String {
    term.trim().to_lowercase()
}

/// Key blinded into a pair record id: `"fr>en"`, or `"auto>en"` when the spoken language is auto-detected.
pub fn pair_key(from: Option<&str>, to: &str) -> String {
    format!("{}>{}", from.unwrap_or("auto"), to)
}

/// Key blinded into a price record id: the model name.
pub fn price_key(model: &str) -> String {
    model.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        assert!(value_matches(InheritNullableNum, &inherit()));
        assert!(value_matches(InheritNullableNum, &Value::Null));
        assert!(value_matches(InheritNullableNum, &serde_json::json!(0.3)));
        assert!(!value_matches(InheritStr, &Value::Null));
        assert!(!value_matches(Int, &serde_json::json!(1.5)));
        assert!(setting_def("polish.model").is_some());
        assert!(setting_def("hotkey.keys").is_none());
        let ids: std::collections::HashSet<_> = SETTINGS.iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), SETTINGS.len());
    }

    #[test]
    fn values_serialise_in_camel_case() {
        let d = DictValue { term: "pnpm".into(), sounds_like: vec!["p n p m".into()] };
        assert_eq!(serde_json::to_value(&d).unwrap(), serde_json::json!({"term": "pnpm", "soundsLike": ["p n p m"]}));
        assert_eq!(pair_key(None, "en"), "auto>en");
        assert_eq!(dict_key("  Kubernetes "), "kubernetes");
    }
}
