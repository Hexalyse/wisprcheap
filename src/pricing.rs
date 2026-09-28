//! Price estimates (USD). Update when providers change their prices.

fn transcription_per_minute(model: &str) -> Option<f64> {
    Some(match model {
        "scribe_v2" | "scribe_v1" => 0.22 / 60.0,
        "gpt-4o-transcribe" => 0.006,
        "gpt-4o-mini-transcribe" => 0.003,
        "gpt-transcribe" => 0.0045,
        "whisper-1" => 0.006,
        _ => return None,
    })
}

/// Scribe keyterm surcharge per audio minute.
const SCRIBE_KEYTERMS_PER_MINUTE: f64 = 0.05 / 60.0;

/// (input, output) USD per million tokens.
fn llm_per_million(model: &str) -> Option<(f64, f64)> {
    Some(match model {
        "gpt-6-luna" => (0.1, 0.5),
        "gpt-6-sol" => (2.0, 10.0),
        "gpt-5.6-luna" => (0.2, 1.2),
        "gpt-5.4-nano" => (0.2, 1.25),
        "gpt-5.4-mini" => (0.75, 4.5),
        "gpt-5-nano" => (0.05, 0.4),
        "gpt-5-mini" => (0.25, 2.0),
        "gpt-4.1-nano" => (0.1, 0.4),
        "gpt-4.1-mini" => (0.4, 1.6),
        "gpt-4o-mini" => (0.15, 0.6),
        _ => return None,
    })
}

pub fn transcription_cost(model: &str, duration_sec: f64, keyterm_count: usize) -> Option<f64> {
    let mut rate = transcription_per_minute(model)?;
    let mut billed = duration_sec;
    if model.starts_with("scribe") && keyterm_count > 0 {
        rate += SCRIBE_KEYTERMS_PER_MINUTE;
        // Above 100 keyterms, ElevenLabs bills each request at least 20 s.
        if keyterm_count > 100 {
            billed = billed.max(20.0);
        }
    }
    Some(billed / 60.0 * rate)
}

pub fn llm_cost(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    let (i, o) = llm_per_million(model)?;
    Some((input_tokens as f64 * i + output_tokens as f64 * o) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scribe_surcharge_and_minimum() {
        let close =
            |a: Option<f64>, e: f64| assert!((a.unwrap() - e).abs() < 1e-12, "{a:?} != {e}");
        close(transcription_cost("scribe_v2", 60.0, 0), 0.22 / 60.0);
        close(transcription_cost("scribe_v2", 60.0, 5), 0.27 / 60.0);
        close(
            transcription_cost("scribe_v2", 5.0, 101),
            (20.0 / 60.0) * (0.27 / 60.0),
        );
        assert_eq!(llm_cost("unknown-model", 100, 100), None);
    }
}
