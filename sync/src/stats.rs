//! Readable statistics of a history entry (SPEC.md section 6): what the server may see.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pricing::{llm_cost, transcription_cost};

pub const MODE_DICTATION: &str = "dictation";
pub const MODE_COMMAND: &str = "command";
pub const STATUS_OK: &str = "ok";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_EMPTY: &str = "empty";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStats {
    /// ISO UTC timestamp of the entry.
    pub ts: String,
    pub mode: String,
    pub duration_sec: f64,
    pub stt_provider: String,
    pub stt_model: String,
    pub stt_ms: u64,
    pub keyterms: u32,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub llm_ms: Option<u64>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    pub words: u64,
    pub status: String,
    #[serde(default)]
    pub retry: bool,
    #[serde(default)]
    pub cost_stt: Option<f64>,
    #[serde(default)]
    pub cost_llm: Option<f64>,
    #[serde(default)]
    pub cost_total: Option<f64>,
}

impl HistoryStats {
    /// Extracts the statistics from a history entry in the desktop `history.jsonl` schema.
    pub fn from_entry(entry: &Value) -> Option<Self> {
        let str_of = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
        let t = entry.get("transcription")?;
        let polish = entry.get("polish").filter(|p| p.is_object());
        let cost = entry.get("costUsd");
        let words = entry.get("words").and_then(Value::as_u64).unwrap_or(0);
        let status = if entry.get("error").is_some_and(|e| !e.is_null()) {
            STATUS_FAILED
        } else if words == 0 {
            STATUS_EMPTY
        } else {
            STATUS_OK
        };
        Some(Self {
            ts: str_of(entry.get("ts"))?,
            mode: str_of(entry.get("mode")).unwrap_or_else(|| MODE_DICTATION.into()),
            duration_sec: entry.get("durationSec").and_then(Value::as_f64).unwrap_or(0.0),
            stt_provider: str_of(t.get("provider")).unwrap_or_default(),
            stt_model: str_of(t.get("model")).unwrap_or_default(),
            stt_ms: t.get("ms").and_then(Value::as_u64).unwrap_or(0),
            keyterms: t.get("keyterms").and_then(Value::as_u64).unwrap_or(0) as u32,
            llm_model: polish.and_then(|p| str_of(p.get("model"))),
            llm_ms: polish.and_then(|p| p.get("ms")).and_then(Value::as_u64),
            input_tokens: polish.and_then(|p| p.get("inputTokens")).and_then(Value::as_u64).unwrap_or(0),
            output_tokens: polish.and_then(|p| p.get("outputTokens")).and_then(Value::as_u64).unwrap_or(0),
            words,
            status: status.into(),
            retry: entry.get("retry").and_then(Value::as_bool).unwrap_or(false),
            cost_stt: cost.and_then(|c| c.get("transcription")).and_then(Value::as_f64),
            cost_llm: cost.and_then(|c| c.get("polish")).and_then(Value::as_f64),
            cost_total: cost.and_then(|c| c.get("total")).and_then(Value::as_f64),
        })
    }

    /// Sanity checks applied by the server before storing uploaded statistics.
    pub fn validate(&self) -> Result<(), String> {
        let finite = |v: f64| v.is_finite() && v >= 0.0;
        if !(20..=40).contains(&self.ts.len()) || !self.ts.ends_with('Z') {
            return Err("ts must be an ISO UTC timestamp".into());
        }
        if self.mode != MODE_DICTATION && self.mode != MODE_COMMAND {
            return Err("unknown mode".into());
        }
        if ![STATUS_OK, STATUS_FAILED, STATUS_EMPTY].contains(&self.status.as_str()) {
            return Err("unknown status".into());
        }
        if !finite(self.duration_sec) || self.duration_sec > 86_400.0 {
            return Err("invalid duration".into());
        }
        for c in [self.cost_stt, self.cost_llm, self.cost_total].into_iter().flatten() {
            if !finite(c) || c > 1_000.0 {
                return Err("invalid cost".into());
            }
        }
        let short = |s: &str| s.len() <= 100;
        if !short(&self.stt_provider) || !short(&self.stt_model) || !self.llm_model.as_deref().is_none_or(short) {
            return Err("model names are limited to 100 characters".into());
        }
        Ok(())
    }

    /// Costs recomputed with the current price table: (transcription, LLM, total).
    pub fn recomputed_costs(&self) -> (Option<f64>, Option<f64>, Option<f64>) {
        let stt = transcription_cost(&self.stt_model, self.duration_sec, self.keyterms as usize);
        let llm = self
            .llm_model
            .as_deref()
            .filter(|_| self.input_tokens + self.output_tokens > 0)
            .and_then(|m| llm_cost(m, self.input_tokens, self.output_tokens));
        let total = stt.map(|s| s + llm.unwrap_or(0.0));
        (stt, llm, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_desktop_entry() {
        let e = serde_json::json!({
            "ts": "2026-09-29T12:34:56.789Z", "durationSec": 3.2,
            "transcription": {"provider": "elevenlabs", "model": "scribe_v2", "ms": 812, "keyterms": 2},
            "polish": {"model": "gpt-6-luna", "ms": 640, "inputTokens": 350, "outputTokens": 20},
            "raw": "hello", "text": "Hello.", "words": 1, "delivered": "pasted",
            "costUsd": {"transcription": 0.0002, "polish": 0.00004, "total": 0.00024}
        });
        let s = HistoryStats::from_entry(&e).unwrap();
        assert_eq!(s.mode, "dictation");
        assert_eq!(s.status, "ok");
        assert_eq!(s.llm_model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(s.input_tokens, 350);
        s.validate().unwrap();
        let (stt, llm, total) = s.recomputed_costs();
        assert!(stt.unwrap() > 0.0 && llm.unwrap() > 0.0 && total.unwrap() > stt.unwrap());
        let failed = serde_json::json!({"ts": "2026-09-29T12:34:56.789Z", "mode": "command",
            "transcription": {"provider": "openai", "model": "x"}, "polish": null, "error": "boom"});
        let f = HistoryStats::from_entry(&failed).unwrap();
        assert_eq!((f.status.as_str(), f.mode.as_str()), ("failed", "command"));
        assert_eq!(f.recomputed_costs(), (None, None, None));
    }
}
