//! Speech-to-text: ElevenLabs Scribe or OpenAI.

use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::multipart::{Form, Part};
use serde_json::Value;

use crate::audio::{encode_wav, pcm_bytes};
use crate::config::{
    Config, DictionaryEntry, ElevenLabsConfig, OpenAiTranscriptionConfig, Provider,
};
use crate::llm::{client, read_error, request_error, trim_slash};

/// Scribe keyterm rules: < 50 chars, at most 5 words, none of `<>{}[]\`, max 1000 terms.
pub fn to_scribe_keyterms(dictionary: &[DictionaryEntry]) -> (Vec<String>, Vec<String>) {
    let mut keyterms = Vec::new();
    let mut skipped = Vec::new();
    for DictionaryEntry { term, .. } in dictionary {
        let ok = term.encode_utf16().count() < 50
            && term.split_whitespace().count() <= 5
            && !term.contains(['<', '>', '{', '}', '[', ']', '\\']);
        if ok && keyterms.len() < 1000 {
            keyterms.push(term.clone());
        } else {
            skipped.push(term.clone());
        }
    }
    (keyterms, skipped)
}

enum Backend {
    ElevenLabs {
        opts: ElevenLabsConfig,
        keyterms: Vec<String>,
    },
    OpenAi {
        opts: OpenAiTranscriptionConfig,
        prompt: String,
    },
}

pub struct Transcriber {
    pub provider: Provider,
    pub model: String,
    /// Number of dictionary terms actually sent to the API (for cost estimates).
    pub keyterm_count: usize,
    language: String,
    timeout: Duration,
    backend: Backend,
}

impl Transcriber {
    pub fn new(config: &Config, dictionary: &[DictionaryEntry]) -> Self {
        let t = &config.transcription;
        let timeout = Duration::from_millis(t.timeout_ms);
        match t.provider {
            Provider::Elevenlabs => {
                let opts = t.elevenlabs.clone();
                let (keyterms, skipped) = if opts.keyterms {
                    to_scribe_keyterms(dictionary)
                } else {
                    (Vec::new(), Vec::new())
                };
                if !skipped.is_empty() {
                    crate::warn!(
                        "[transcribe] Skipped dictionary entries not valid as Scribe keyterms: {}",
                        skipped.join(", ")
                    );
                }
                if keyterms.len() > 100 {
                    crate::warn!(
                        "[transcribe] {} keyterms: ElevenLabs bills each request at least 20 s above 100 keyterms.",
                        keyterms.len()
                    );
                }
                Self {
                    provider: Provider::Elevenlabs,
                    model: opts.model.clone(),
                    keyterm_count: keyterms.len(),
                    language: t.language.clone(),
                    timeout,
                    backend: Backend::ElevenLabs { opts, keyterms },
                }
            }
            Provider::Openai => {
                let opts = t.openai.clone();
                let terms: Vec<&str> = dictionary.iter().map(|d| d.term.as_str()).collect();
                let vocabulary = if terms.is_empty() {
                    String::new()
                } else {
                    format!("Vocabulary: {}.", terms.join(", "))
                };
                let prompt = [opts.prompt.trim().to_string(), vocabulary]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                Self {
                    provider: Provider::Openai,
                    model: opts.model.clone(),
                    keyterm_count: 0,
                    language: t.language.clone(),
                    timeout,
                    backend: Backend::OpenAi { opts, prompt },
                }
            }
        }
    }

    /// `language`: spoken language for this request (overrides the config), or None to use the config.
    pub async fn transcribe(&self, pcm: &[i16], language: Option<&str>) -> Result<String> {
        let lang = language.unwrap_or(&self.language).to_string();
        let res = match &self.backend {
            Backend::ElevenLabs { opts, keyterms } => {
                // Raw 16 kHz mono s16le is accepted directly and gives lower latency than an encoded file.
                let file = Part::bytes(pcm_bytes(pcm))
                    .file_name("audio.pcm")
                    .mime_str("application/octet-stream")?;
                let mut form = Form::new()
                    .text("model_id", opts.model.clone())
                    .part("file", file)
                    .text("file_format", "pcm_s16le_16")
                    .text("tag_audio_events", "false")
                    .text("timestamps_granularity", "none");
                if lang != "auto" {
                    form = form.text("language_code", lang.clone());
                }
                if opts.no_verbatim {
                    form = form.text("no_verbatim", "true");
                }
                for term in keyterms {
                    form = form.text("keyterms", term.clone());
                }
                let res = client()
                    .post(format!("{}/v1/speech-to-text", trim_slash(&opts.base_url)))
                    .timeout(self.timeout)
                    .header("xi-api-key", opts.api_key.clone().unwrap_or_default())
                    .multipart(form)
                    .send()
                    .await
                    .map_err(request_error)?;
                if !res.status().is_success() {
                    return Err(anyhow!("ElevenLabs: {}", read_error(res).await));
                }
                res
            }
            Backend::OpenAi { opts, prompt } => {
                let file = Part::bytes(encode_wav(pcm))
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?;
                let mut form = Form::new()
                    .text("model", opts.model.clone())
                    .part("file", file)
                    .text("response_format", "json")
                    .text("temperature", "0");
                if lang != "auto" {
                    form = form.text("language", lang.clone());
                }
                if !prompt.is_empty() {
                    form = form.text("prompt", prompt.clone());
                }
                let res = client()
                    .post(format!(
                        "{}/audio/transcriptions",
                        trim_slash(&opts.base_url)
                    ))
                    .timeout(self.timeout)
                    .bearer_auth(opts.api_key.clone().unwrap_or_default())
                    .multipart(form)
                    .send()
                    .await
                    .map_err(request_error)?;
                if !res.status().is_success() {
                    return Err(anyhow!("OpenAI transcription: {}", read_error(res).await));
                }
                res
            }
        };
        let json: Value = res.json().await.map_err(request_error)?;
        Ok(json["text"].as_str().unwrap_or_default().trim().to_string())
    }
}
