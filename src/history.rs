//! Dictation history (`history.jsonl`), monthly totals and saved failed audio.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::audio::encode_wav;
use crate::config::HistoryConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionInfo {
    pub provider: String,
    pub model: String,
    pub ms: u64,
    pub keyterms: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishInfo {
    pub model: String,
    pub ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Costs {
    pub transcription: Option<f64>,
    pub polish: Option<f64>,
    pub total: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivered {
    Pasted,
    Clipboard,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryEntry {
    pub ts: String,
    /// Unique id (UUID v4). Older entries have none (sync derives one from the device and `ts`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Sync device id of the device that recorded the entry (only when sync is set up).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// "command" for command mode; absent for dictations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub duration_sec: f64,
    pub transcription: TranscriptionInfo,
    pub polish: Option<PolishInfo>,
    pub raw: String,
    pub text: String,
    pub words: usize,
    pub delivered: Option<Delivered>,
    pub cost_usd: Costs,
    /// True when this entry is a retry of a failed recording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<bool>,
    /// Translation pair used, e.g. "fr>en".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
    /// Word count of a transcript pasted without polish because it was shorter than polish.minWords.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polish_skipped: Option<usize>,
    /// Command mode: the text that was selected when the command was spoken (null: nothing selected).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some",
        default
    )]
    pub selection: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_file: Option<String>,
}

fn deserialize_some<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonthTotals {
    pub month: String,
    pub cost_usd: f64,
    pub words: usize,
    pub entries: usize,
}

pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// "2026-09" in local time.
pub fn month_key(date: DateTime<Local>) -> String {
    format!("{}-{:02}", date.year(), date.month())
}

/// ISO timestamp like JavaScript's `toISOString()`.
pub fn iso_timestamp(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn entry_month(entry: &HistoryEntry) -> Option<String> {
    DateTime::parse_from_rfc3339(&entry.ts)
        .ok()
        .map(|d| month_key(d.with_timezone(&Local)))
}

/// Totals for one month. Failed transcriptions have no cost and are skipped by construction.
pub fn totals_for(entries: &[HistoryEntry], month: &str) -> MonthTotals {
    let mut totals = MonthTotals {
        month: month.to_string(),
        ..Default::default()
    };
    for e in entries {
        if entry_month(e).as_deref() != Some(month) {
            continue;
        }
        totals.cost_usd += e.cost_usd.total.or(e.cost_usd.transcription).unwrap_or(0.0);
        totals.words += e.words;
        if e.error.is_none() && e.words > 0 {
            totals.entries += 1;
        }
    }
    totals
}

pub struct History {
    file: PathBuf,
    audio_dir: PathBuf,
    enabled: bool,
    save_failed_audio: bool,
    month: Mutex<MonthTotals>,
}

impl History {
    pub fn new(opts: &HistoryConfig, base_dir: &Path) -> Self {
        let file = crate::paths::resolve(base_dir, &opts.path);
        let audio_dir = crate::paths::resolve(base_dir, &opts.failed_audio_dir);
        let month = totals_for(&read_history(&file), &month_key(Local::now()));
        Self {
            file,
            audio_dir,
            enabled: opts.enabled,
            save_failed_audio: opts.save_failed_audio,
            month: Mutex::new(month),
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Running totals for the current month (from the file at startup, then updated on each append).
    pub fn current_month(&self) -> MonthTotals {
        let mut month = self.month.lock().unwrap();
        let key = month_key(Local::now());
        if month.month != key {
            *month = MonthTotals {
                month: key,
                ..Default::default()
            };
        }
        month.clone()
    }

    pub fn append(&self, entry: &HistoryEntry) {
        let current = self.current_month();
        let added = totals_for(std::slice::from_ref(entry), &current.month);
        {
            let mut month = self.month.lock().unwrap();
            month.cost_usd += added.cost_usd;
            month.words += added.words;
            month.entries += added.entries;
        }
        if !self.enabled {
            return;
        }
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.file.parent() {
                fs::create_dir_all(dir)?;
            }
            let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file)?;
            writeln!(f, "{line}")
        })();
        if let Err(e) = result {
            crate::warn!("[history] write failed: {e}");
        }
    }

    /// Save audio whose transcription failed, so it can be re-transcribed by hand.
    pub fn save_failed_audio(&self, pcm: &[i16], ts: DateTime<Utc>) -> Option<String> {
        if !self.save_failed_audio {
            return None;
        }
        let name = format!("failed-{}.wav", iso_timestamp(ts).replace([':', '.'], "-"));
        let file = self.audio_dir.join(name);
        let result =
            fs::create_dir_all(&self.audio_dir).and_then(|_| fs::write(&file, encode_wav(pcm)));
        match result {
            Ok(()) => Some(file.to_string_lossy().into_owned()),
            Err(e) => {
                crate::warn!("[history] could not save audio: {e}");
                None
            }
        }
    }
}

pub fn read_history(file: &Path) -> Vec<HistoryEntry> {
    let Ok(content) = fs::read_to_string(file) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(
        ts: DateTime<Local>,
        total: Option<f64>,
        words: usize,
        error: Option<&str>,
    ) -> HistoryEntry {
        HistoryEntry {
            ts: iso_timestamp(ts.with_timezone(&Utc)),
            cost_usd: Costs {
                transcription: total,
                polish: None,
                total,
            },
            words,
            error: error.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn month_totals_use_local_time() {
        let at = |m, d, h, min| Local.with_ymd_and_hms(2026, m, d, h, min, 0).unwrap();
        let month = month_key(at(9, 15, 0, 0));
        let totals = totals_for(
            &[
                entry(at(9, 1, 0, 30), Some(0.01), 10, None),
                entry(at(9, 20, 0, 0), Some(0.02), 5, None),
                entry(at(9, 21, 0, 0), None, 0, Some("HTTP 500")),
                entry(at(10, 1, 0, 0), Some(1.0), 100, None),
            ],
            &month,
        );
        assert_eq!(month, "2026-09");
        assert_eq!((totals.cost_usd * 100.0).round() / 100.0, 0.03);
        assert_eq!(totals.words, 15);
        assert_eq!(totals.entries, 2);
    }

    #[test]
    fn selection_null_round_trips() {
        let e = HistoryEntry {
            selection: Some(None),
            ..Default::default()
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"selection\":null"));
        let back: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.selection, Some(None));
    }
}
