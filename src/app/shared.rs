//! State shared by the event handler (actor) and the processing queue.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, Local};
use tokio::sync::mpsc::UnboundedSender;

use crate::command::Commander;
use crate::config::{Config, DictionaryEntry, LlmOptions, LoadedConfig, TranslationPair};
use crate::history::{History, HistoryEntry, count_words};
use crate::hotkey::{Mode, State as HotkeyState};
use crate::icons::IconName;
use crate::output::SharedHotkey;
use crate::overlay::{Feedback, OverlayStatus, Recording};
use crate::polish::Polisher;
use crate::sounds::{Cue, Sounds};
use crate::state::AppState;
use crate::sync::SyncHandle;
use crate::transcribe::Transcriber;
use crate::tray::Ui;

use super::{AppMsg, Job};

/// Everything rebuilt when the config file changes.
pub struct Pipeline {
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub dictionary: Vec<DictionaryEntry>,
    pub transcriber: Transcriber,
    pub polisher: Option<Polisher>,
    pub commander: Option<Commander>,
    pub history: History,
    pub pairs: Vec<TranslationPair>,
    pub translators: HashMap<String, Polisher>,
}

pub fn polish_llm(config: &Config) -> LlmOptions {
    let p = &config.polish;
    LlmOptions {
        api_key: p.api_key.clone(),
        base_url: p.base_url.clone(),
        model: p.model.clone(),
        reasoning_effort: p.reasoning_effort.clone(),
        temperature: p.temperature,
        timeout_ms: p.timeout_ms,
    }
}

impl Pipeline {
    pub fn build(loaded: &LoadedConfig) -> Self {
        let config = loaded.config.clone();
        let dictionary = loaded.dictionary.clone();
        let transcriber = Transcriber::new(&config, &dictionary);
        let polisher = config.polish.enabled.then(|| {
            Polisher::new(
                &polish_llm(&config),
                &config.polish.instructions,
                &dictionary,
                None,
            )
        });
        let commander = (!config.hotkey.command_keys.is_empty())
            .then(|| Commander::new(&loaded.command_llm, &dictionary));
        let history = History::new(&config.history, &loaded.base_dir);
        let translators = loaded
            .translation_pairs
            .iter()
            .map(|pair| {
                (
                    pair.id.clone(),
                    Polisher::new(
                        &loaded.translation_llm,
                        &config.polish.instructions,
                        &dictionary,
                        Some(&pair.to_name),
                    ),
                )
            })
            .collect();
        Self {
            config_path: loaded.config_path.clone(),
            dictionary,
            transcriber,
            polisher,
            commander,
            history,
            pairs: loaded.translation_pairs.clone(),
            translators,
            config,
        }
    }
}

#[derive(Clone)]
pub struct LastFailed {
    pub pcm: Arc<Vec<i16>>,
    pub ts: DateTime<Local>,
}

pub struct Status {
    pub paused: bool,
    pub pending: usize,
    /// Pending jobs that process a recording (dictation, command), shown by the overlay.
    pub audio_jobs: usize,
    pub recording: bool,
    pub recording_mode: Mode,
    pub busy_label: String,
    pub state: AppState,
    pub last_text: Option<String>,
    pub last_failed: Option<LastFailed>,
}

pub struct Shared {
    pub pipeline: RwLock<Arc<Pipeline>>,
    pub status: Mutex<Status>,
    pub hotkey: SharedHotkey,
    pub sounds: Mutex<Sounds>,
    pub ui: Option<Ui>,
    pub base_dir: PathBuf,
    pub app_tx: UnboundedSender<AppMsg>,
    pub job_tx: UnboundedSender<Job>,
    pub sync: SyncHandle,
}

/// JavaScript-like number formatting for log lines (600 -> "600", -Infinity -> "-Infinity").
pub fn js_number(n: f64) -> String {
    if n == f64::NEG_INFINITY {
        "-Infinity".into()
    } else if n == f64::INFINITY {
        "Infinity".into()
    } else {
        format!("{n}")
    }
}

pub fn fixed(n: f64, digits: usize) -> String {
    if n.is_infinite() {
        js_number(n)
    } else {
        format!("{n:.digits$}")
    }
}

impl Shared {
    pub fn pipeline(&self) -> Arc<Pipeline> {
        self.pipeline.read().unwrap().clone()
    }

    pub fn play(&self, cue: Cue) {
        self.sounds.lock().unwrap().play(cue);
    }

    /// Error notification (config: notifications.errors). Clicking it opens the log.
    pub fn notify_error(&self, title: &str, message: &str) {
        if self.pipeline().config.notifications.errors
            && let Some(ui) = &self.ui
        {
            ui.notify_error(title, message);
        }
    }

    pub fn current_pair(&self, p: &Pipeline) -> Option<TranslationPair> {
        let id = self.status.lock().unwrap().state.translation.clone()?;
        p.pairs.iter().find(|pair| pair.id == id).cloned()
    }

    /// Install a new pipeline (config reload).
    pub fn install_pipeline(&self, pipeline: Pipeline) {
        *self.pipeline.write().unwrap() = Arc::new(pipeline);
        self.pipeline_changed();
    }

    /// A pair removed from the config turns translation off; then refresh the tray.
    pub fn pipeline_changed(&self) {
        let p = self.pipeline();
        {
            let mut status = self.status.lock().unwrap();
            if let Some(id) = &status.state.translation
                && !p.translators.contains_key(id)
            {
                status.state.translation = None;
            }
        }
        self.refresh_tray();
    }

    pub fn update_status(&self) {
        let Some(ui) = &self.ui else {
            return;
        };
        let p = self.pipeline();
        let (label, hands_free) = {
            let h = self.hotkey.lock().unwrap();
            (h.label(), h.state() == HotkeyState::HandsFree)
        };
        let pair = self.current_pair(&p);
        let translating = pair
            .map(|p| format!(" (to {})", p.to_name))
            .unwrap_or_default();
        let (icon, text, overlay) = {
            let st = self.status.lock().unwrap();
            let overlay = OverlayStatus {
                enabled: p.config.overlay.enabled,
                recording: st.recording.then_some(Recording {
                    mode: st.recording_mode,
                    hands_free,
                }),
                busy: st.audio_jobs > 0,
            };
            let (icon, text) = if st.recording {
                let text = if st.recording_mode == Mode::Command {
                    "Recording command...".to_string()
                } else {
                    format!("Recording{translating}...")
                };
                (IconName::Recording, text)
            } else if st.pending > 0 {
                (IconName::Processing, st.busy_label.clone())
            } else if st.paused {
                (IconName::Paused, "Paused".to_string())
            } else {
                (IconName::Idle, format!("Ready{translating} - hold {label}"))
            };
            (icon, text, overlay)
        };
        ui.set_state(icon, &text);
        ui.set_overlay(overlay);
    }

    /// Show how a recording ended on the overlay (when it's enabled).
    pub fn overlay_feedback(&self, feedback: Feedback) {
        if self.pipeline().config.overlay.enabled
            && let Some(ui) = &self.ui
        {
            ui.overlay_feedback(feedback);
        }
    }

    /// Push everything the tray menu shows that doesn't depend on the recording state.
    pub fn refresh_tray(&self) {
        let Some(ui) = &self.ui else {
            return;
        };
        let p = self.pipeline();
        let pair = self.current_pair(&p);
        let selected = pair
            .and_then(|pair| p.pairs.iter().position(|x| x.id == pair.id))
            .map(|i| i as i32)
            .unwrap_or(-1);
        ui.set_translations(p.pairs.iter().map(|x| x.label.clone()).collect(), selected);
    }

    pub fn set_last_failed(&self, value: Option<LastFailed>) {
        let available = value.is_some();
        self.status.lock().unwrap().last_failed = value;
        if let Some(ui) = &self.ui {
            ui.set_failed_available(available);
        }
    }

    pub fn set_last_text(&self, text: &str) {
        self.status.lock().unwrap().last_text = Some(text.to_string());
        if let Some(ui) = &self.ui {
            ui.set_last_available(true);
        }
    }

    pub fn enqueue(&self, job: Job) {
        {
            let mut st = self.status.lock().unwrap();
            st.pending += 1;
            if job.has_audio() {
                st.audio_jobs += 1;
            }
        }
        let _ = self.job_tx.send(job);
    }

    /// Write the entry to the history and notify sync.
    pub fn record(&self, p: &Pipeline, entry: &HistoryEntry) {
        let mut entry = entry.clone();
        if entry.id.is_none() {
            entry.id = Some(uuid::Uuid::new_v4().to_string());
        }
        if entry.device.is_none() {
            entry.device = self.sync.device_id();
        }
        p.history.append(&entry);
        self.sync.history_appended();
    }

    pub fn finish_entry(&self, p: &Pipeline, entry: &mut HistoryEntry, text: &str) {
        entry.text = text.to_string();
        entry.words = count_words(text);
        let round = |n: Option<f64>| n.map(|v| (v * 1e7).round() / 1e7);
        let (transcription, polish) = (entry.cost_usd.transcription, entry.cost_usd.polish);
        entry.cost_usd.transcription = round(transcription);
        entry.cost_usd.polish = round(polish);
        entry.cost_usd.total = round(transcription.map(|t| t + polish.unwrap_or(0.0)));
        self.record(p, entry);
    }

    pub fn schedule_reload(&self) {
        let _ = self.app_tx.send(AppMsg::ScheduleReload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers() {
        assert_eq!(js_number(-55.0), "-55");
        assert_eq!(js_number(600.5), "600.5");
        assert_eq!(fixed(f64::NEG_INFINITY, 1), "-Infinity");
        assert_eq!(fixed(-60.04, 1), "-60.0");
    }
}
