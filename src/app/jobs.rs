//! Processing jobs, run one at a time so pastes land in the order they were spoken.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::{Local, Utc};

use crate::audio::{loudest_window_db, pcm_duration_ms};
use crate::clipboard;
use crate::config::ensure_config_file;
use crate::dictionary::{AddTermResult, add_dictionary_term};
use crate::history::{
    Delivered, HistoryEntry, PolishInfo, TranscriptionInfo, count_words, iso_timestamp,
};
use crate::llm::is_network_error;
use crate::output::{Selection, capture_selection, deliver, restore_clipboard};
use crate::overlay::Feedback;
use crate::pricing::{llm_cost_with, transcription_cost_with};
use crate::sounds::Cue;
use crate::{error, info, say};

use super::shared::{LastFailed, Pipeline, Shared, fixed, js_number};

pub enum AddWordSource {
    Selection,
    Clipboard,
}

pub enum Job {
    Dictation { pcm: Arc<Vec<i16>>, retry: bool },
    Command { pcm: Arc<Vec<i16>> },
    AddWord(AddWordSource),
}

impl Job {
    /// Processes a recording (the overlay shows these).
    pub fn has_audio(&self) -> bool {
        !matches!(self, Job::AddWord(_))
    }
}

pub async fn worker(shared: Arc<Shared>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = rx.recv().await {
        let sh = shared.clone();
        let audio = job.has_audio();
        // Run in its own task so a bug in one job doesn't stop the queue.
        let result = tokio::spawn(async move {
            match job {
                Job::Dictation { pcm, retry } => process_dictation(&sh, pcm, retry).await,
                Job::Command { pcm } => process_command(&sh, pcm).await,
                Job::AddWord(source) => add_word(&sh, source).await,
            }
        })
        .await;
        if let Err(e) = result {
            error!("Unexpected error: {e}");
            if audio {
                shared.overlay_feedback(Feedback::Error);
            }
        }
        {
            let mut st = shared.status.lock().unwrap();
            st.pending = st.pending.saturating_sub(1);
            if audio {
                st.audio_jobs = st.audio_jobs.saturating_sub(1);
            }
            st.busy_label = "Transcribing...".into();
        }
        shared.update_status();
    }
}

/// Common checks before sending audio anywhere. Returns false (and logs why) when it should be dropped.
fn is_usable_audio(sh: &Shared, p: &Pipeline, pcm: &[i16]) -> bool {
    let duration_ms = pcm_duration_ms(pcm.len());
    if duration_ms < p.config.recording.min_duration_ms as f64 {
        info!("Discarded: too short ({} ms).", duration_ms.round());
        return false;
    }
    let level = loudest_window_db(pcm);
    let threshold = p.config.recording.silence_threshold_db;
    if level < threshold {
        info!(
            "Discarded: no speech detected (peak {} dBFS < {}).",
            fixed(level, 1),
            js_number(threshold)
        );
        sh.play(Cue::Cancel);
        sh.overlay_feedback(Feedback::Discarded);
        return false;
    }
    true
}

/// The overlay's feedback for a delivery.
fn delivered_feedback(delivered: Option<Delivered>) -> Feedback {
    match delivered {
        Some(Delivered::Pasted) => Feedback::Pasted,
        Some(Delivered::Clipboard) => Feedback::Copied,
        None => Feedback::Error,
    }
}

fn new_entry(p: &Pipeline, pcm: &[i16], started_at: chrono::DateTime<Utc>) -> HistoryEntry {
    HistoryEntry {
        ts: iso_timestamp(started_at),
        id: Some(uuid::Uuid::new_v4().to_string()),
        duration_sec: (pcm_duration_ms(pcm.len()) / 1000.0 * 100.0).round() / 100.0,
        transcription: TranscriptionInfo {
            provider: p.transcriber.provider.as_str().to_string(),
            model: p.transcriber.model.clone(),
            ms: 0,
            keyterms: p.transcriber.keyterm_count,
        },
        ..Default::default()
    }
}

/// Transcribe with one retry on network errors/timeouts. Fills the entry's transcription fields.
async fn transcribe(
    p: &Pipeline,
    pcm: &[i16],
    entry: &mut HistoryEntry,
    language: Option<&str>,
) -> Result<String> {
    let t0 = Instant::now();
    let raw = match p.transcriber.transcribe(pcm, language).await {
        Err(e) if is_network_error(&e) => {
            info!("Transcription request failed, retrying once...");
            p.transcriber.transcribe(pcm, language).await?
        }
        other => other?,
    };
    entry.transcription.ms = t0.elapsed().as_millis() as u64;
    entry.raw = raw.clone();
    entry.cost_usd.transcription = transcription_cost_with(
        &p.config.pricing.overrides,
        &p.transcriber.model,
        entry.duration_sec,
        p.transcriber.keyterm_count,
    );
    Ok(raw)
}

fn timing_summary(entry: &HistoryEntry, llm_label: &str) -> String {
    let llm = if let Some(polish) = &entry.polish {
        format!(" | {llm_label} {} ms", polish.ms)
    } else if let Some(words) = entry.polish_skipped {
        format!(" | polish skipped ({words} words)")
    } else {
        String::new()
    };
    let cost = entry
        .cost_usd
        .total
        .map(|t| format!(" | ~${t:.5}"))
        .unwrap_or_default();
    format!(
        "{:.1}s audio | stt {} ms{llm}{cost}",
        entry.duration_sec, entry.transcription.ms
    )
}

/// Dictation: transcribe, polish (or translate), paste. With `retry` (from the tray), the text only goes to
/// the clipboard, since focus is on the tray menu rather than on a text field.
pub async fn process_dictation(sh: &Arc<Shared>, pcm: Arc<Vec<i16>>, retry: bool) {
    let started_at = Utc::now();
    let p = sh.pipeline();
    if !is_usable_audio(sh, &p, &pcm) {
        return;
    }
    let mut entry = new_entry(&p, &pcm, started_at);
    if retry {
        entry.retry = Some(true);
    }
    let pair = sh.current_pair(&p);
    let translator = pair.as_ref().and_then(|pair| p.translators.get(&pair.id));
    if let Some(pair) = &pair {
        entry.translation = Some(pair.id.clone());
    }

    let language = pair.as_ref().and_then(|pair| pair.from.clone());
    let raw = match transcribe(&p, &pcm, &mut entry, language.as_deref()).await {
        Ok(raw) => raw,
        Err(e) => {
            let message = e.to_string();
            entry.error = Some(message.clone());
            if !retry {
                entry.audio_file = p.history.save_failed_audio(&pcm, started_at);
            }
            sh.record(&p, &entry);
            sh.set_last_failed(Some(LastFailed {
                pcm: pcm.clone(),
                ts: started_at.with_timezone(&Local),
            }));
            sh.play(Cue::Error);
            sh.overlay_feedback(Feedback::Error);
            let saved = entry
                .audio_file
                .as_ref()
                .map(|f| format!("\n  Audio saved to {f}"))
                .unwrap_or_default();
            error!(
                "Transcription failed: {message}{saved}\n  Use \"Retry last failed\" in the tray menu to try again."
            );
            sh.notify_error(
                "Transcription failed",
                &format!("{message}\nUse \"Retry last failed\" in the tray menu to try again."),
            );
            return;
        }
    };
    if retry {
        sh.set_last_failed(None);
    }

    if raw.is_empty() {
        info!("Discarded: the transcript is empty.");
        sh.record(&p, &entry);
        sh.play(Cue::Cancel);
        sh.overlay_feedback(Feedback::Discarded);
        return;
    }

    // Translate, or polish (short transcripts skip polish). Falls back to the raw transcript on any failure.
    let mut text = raw.clone();
    let words = count_words(&raw);
    let min_words = p.config.polish.min_words as usize;
    let skip_polish = translator.is_none() && min_words > 0 && words < min_words;
    let llm = translator.or(if skip_polish {
        None
    } else {
        p.polisher.as_ref()
    });
    if skip_polish && p.polisher.is_some() {
        entry.polish_skipped = Some(words);
    }
    if let Some(llm) = llm {
        let t1 = Instant::now();
        let mut info = PolishInfo {
            model: llm.model.clone(),
            ..Default::default()
        };
        match llm.polish(&raw).await {
            Ok(result) => {
                text = result.text;
                info.input_tokens = result.input_tokens;
                info.output_tokens = result.output_tokens;
                entry.cost_usd.polish = llm_cost_with(
                    &p.config.pricing.overrides,
                    &llm.model,
                    result.input_tokens,
                    result.output_tokens,
                );
            }
            Err(e) => {
                let message = e.to_string();
                error!("{message}\n  Using the raw transcript.");
                if translator.is_some() {
                    sh.notify_error(
                        "Translation failed",
                        &format!("{message}\nThe untranslated text was pasted."),
                    );
                }
                info.error = Some(message);
            }
        }
        info.ms = t1.elapsed().as_millis() as u64;
        entry.polish = Some(info);
    }

    sh.set_last_text(&text);
    let output = if p.config.output.trailing_space {
        format!("{text} ")
    } else {
        text.clone()
    };
    let mut output_opts = p.config.output.clone();
    if retry {
        output_opts.paste = false;
    }
    match deliver(&output, &output_opts, &sh.hotkey).await {
        Ok(delivered) => entry.delivered = Some(delivered),
        Err(e) => {
            sh.play(Cue::Error);
            error!("Could not copy/paste the text: {e}");
            sh.notify_error("Could not paste", &e.to_string());
        }
    }
    sh.overlay_feedback(delivered_feedback(entry.delivered));
    sh.finish_entry(&p, &mut entry, &text);

    let action = if retry {
        "Retry succeeded, copied to the clipboard"
    } else if entry.delivered == Some(Delivered::Pasted) {
        "Pasted"
    } else {
        "Copied"
    };
    let llm_label = match (&pair, translator) {
        (Some(pair), Some(_)) => format!("translate to {}", pair.to),
        _ => "polish".to_string(),
    };
    info!("{action} ({})", timing_summary(&entry, &llm_label));
    if retry {
        sh.play(Cue::Added);
    }
    if text != raw {
        say!("  raw:  {raw}");
    }
    say!("  text: {text}");
}

/// Command mode: copy the selection, transcribe the spoken instruction, let the LLM rewrite or write, paste.
pub async fn process_command(sh: &Arc<Shared>, pcm: Arc<Vec<i16>>) {
    let started_at = Utc::now();
    let p = sh.pipeline();
    let Some(commander) = p.commander.as_ref() else {
        return;
    };
    if !is_usable_audio(sh, &p, &pcm) {
        return;
    }
    sh.status.lock().unwrap().busy_label = "Running command...".into();
    sh.update_status();
    let mut entry = new_entry(&p, &pcm, started_at);
    entry.mode = Some("command".into());

    // Copy the selection while the instruction is being transcribed.
    let hotkey = sh.hotkey.clone();
    let selection_task = tokio::spawn(async move { capture_selection(&hotkey).await });
    let transcribed = transcribe(&p, &pcm, &mut entry, None).await;
    let selection = selection_task.await.unwrap_or(Selection {
        text: None,
        previous: None,
    });
    let instruction = match transcribed {
        Ok(i) => i,
        Err(e) => {
            restore_clipboard(selection.previous.as_deref()).await;
            let message = e.to_string();
            entry.error = Some(message.clone());
            sh.record(&p, &entry);
            sh.play(Cue::Error);
            sh.overlay_feedback(Feedback::Error);
            error!("Command: transcription failed: {message}");
            sh.notify_error(
                "Command failed",
                &format!("Transcription failed: {message}"),
            );
            return;
        }
    };
    entry.selection = Some(selection.text.clone());

    if instruction.is_empty() {
        restore_clipboard(selection.previous.as_deref()).await;
        info!("Command discarded: the instruction is empty.");
        sh.record(&p, &entry);
        sh.play(Cue::Cancel);
        sh.overlay_feedback(Feedback::Discarded);
        return;
    }

    let t1 = Instant::now();
    let mut info = PolishInfo {
        model: commander.model.clone(),
        ..Default::default()
    };
    let text = match commander.run(&instruction, selection.text.as_deref()).await {
        Ok(result) => {
            info.input_tokens = result.input_tokens;
            info.output_tokens = result.output_tokens;
            entry.cost_usd.polish = llm_cost_with(
                &p.config.pricing.overrides,
                &commander.model,
                result.input_tokens,
                result.output_tokens,
            );
            result.text
        }
        Err(e) => {
            let message = e.to_string();
            info.ms = t1.elapsed().as_millis() as u64;
            info.error = Some(message.clone());
            entry.polish = Some(info);
            entry.error = Some(message.clone());
            restore_clipboard(selection.previous.as_deref()).await;
            sh.record(&p, &entry);
            sh.play(Cue::Error);
            sh.overlay_feedback(Feedback::Error);
            error!("{message}\n  instruction: {instruction}");
            sh.notify_error("Command failed", &message);
            return;
        }
    };
    info.ms = t1.elapsed().as_millis() as u64;
    entry.polish = Some(info);

    sh.set_last_text(&text);
    match deliver(&text, &p.config.output, &sh.hotkey).await {
        Ok(delivered) => entry.delivered = Some(delivered),
        Err(e) => {
            sh.play(Cue::Error);
            error!("Could not copy/paste the text: {e}");
            sh.notify_error("Could not paste", &e.to_string());
        }
    }
    sh.overlay_feedback(delivered_feedback(entry.delivered));
    sh.finish_entry(&p, &mut entry, &text);

    let target = match &selection.text {
        Some(sel) => format!(
            "replaced the selection ({} chars)",
            sel.encode_utf16().count()
        ),
        None => "inserted at the cursor".to_string(),
    };
    let what = if entry.delivered == Some(Delivered::Pasted) {
        target
    } else {
        "result copied".to_string()
    };
    info!("Command {what} ({})", timing_summary(&entry, "llm"));
    say!("  instruction: {instruction}");
    let result = if text.encode_utf16().count() > 300 {
        format!(
            "{}...",
            String::from_utf16_lossy(&text.encode_utf16().take(300).collect::<Vec<_>>())
        )
    } else {
        text.clone()
    };
    say!("  result:      {result}");
}

/// Add the selected text (shortcut) or the clipboard content (tray) to the dictionary in config.yaml.
pub async fn add_word(sh: &Arc<Shared>, source: AddWordSource) {
    let text = match source {
        AddWordSource::Selection => {
            let selection = capture_selection(&sh.hotkey).await;
            restore_clipboard(selection.previous.as_deref()).await;
            selection.text
        }
        AddWordSource::Clipboard => clipboard::read().await.ok(),
    };
    let p = sh.pipeline();
    let result = ensure_config_file(p.config_path.as_deref()).and_then(|file| {
        add_dictionary_term(&file, text.as_deref().unwrap_or_default()).map(|r| (file, r))
    });
    match result {
        Ok((_, (term, AddTermResult::Exists))) => {
            info!("\"{term}\" is already in the dictionary.");
            sh.play(Cue::Cancel);
        }
        Ok((file, (term, AddTermResult::Added))) => {
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            info!(
                "Added \"{term}\" to the dictionary. Add soundsLike hints in {name} if it's often misheard."
            );
            sh.play(Cue::Added);
            sh.schedule_reload(); // the file watcher would also catch it; this covers a watcher that failed to start
        }
        Err(e) => {
            error!("Could not add to the dictionary: {e}");
            sh.play(Cue::Error);
            sh.notify_error("Not added to the dictionary", &e.to_string());
        }
    }
}

pub async fn copy_last(sh: &Arc<Shared>) {
    let Some(text) = sh.status.lock().unwrap().last_text.clone() else {
        return;
    };
    match clipboard::write(text).await {
        Ok(()) => info!("Copied the last dictation to the clipboard."),
        Err(e) => error!("Could not copy to the clipboard: {e}"),
    }
}
