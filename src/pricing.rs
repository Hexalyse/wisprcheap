//! Price estimates: shared with the sync server, see the `wisprcheap-sync` crate. The functions
//! below also apply the user's `pricing.overrides` (same rules as the Android app).

pub use wisprcheap_sync::pricing::*;
use wisprcheap_sync::profile::PriceValue;

/// Scribe keyterm surcharge per audio minute (kept in sync with the shared table).
const SCRIBE_KEYTERMS_PER_MINUTE: f64 = 0.05 / 60.0;

fn find<'a>(overrides: &'a [PriceValue], model: &str) -> Option<&'a PriceValue> {
    overrides.iter().find(|o| o.model.trim() == model)
}

/// Transcription cost; an override's `perMinute` replaces the built-in rate.
pub fn transcription_cost_with(
    overrides: &[PriceValue],
    model: &str,
    duration_sec: f64,
    keyterm_count: usize,
) -> Option<f64> {
    let Some(mut rate) = find(overrides, model).and_then(|o| o.per_minute) else {
        return transcription_cost(model, duration_sec, keyterm_count);
    };
    let mut billed = duration_sec;
    if model.starts_with("scribe") && keyterm_count > 0 {
        rate += SCRIBE_KEYTERMS_PER_MINUTE;
        if keyterm_count > 100 {
            billed = billed.max(20.0);
        }
    }
    Some(billed / 60.0 * rate)
}

/// LLM cost; an override applies when it sets both `inputPerM` and `outputPerM`.
pub fn llm_cost_with(
    overrides: &[PriceValue],
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> Option<f64> {
    match find(overrides, model) {
        Some(PriceValue {
            input_per_m: Some(i),
            output_per_m: Some(o),
            ..
        }) => Some((input_tokens as f64 * i + output_tokens as f64 * o) / 1e6),
        _ => llm_cost(model, input_tokens, output_tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_win() {
        let o = vec![
            PriceValue {
                model: "my-stt".into(),
                per_minute: Some(0.01),
                input_per_m: None,
                output_per_m: None,
            },
            PriceValue {
                model: "gpt-6-luna".into(),
                per_minute: None,
                input_per_m: Some(1.0),
                output_per_m: Some(2.0),
            },
        ];
        assert_eq!(transcription_cost_with(&o, "my-stt", 60.0, 0), Some(0.01));
        assert_eq!(transcription_cost_with(&o, "unknown", 60.0, 0), None);
        assert_eq!(llm_cost_with(&o, "gpt-6-luna", 1_000_000, 1_000_000), Some(3.0));
        assert_eq!(
            llm_cost_with(&[], "gpt-6-luna", 1_000_000, 0),
            llm_cost("gpt-6-luna", 1_000_000, 0)
        );
        assert!(
            (transcription_cost_with(&[], "scribe_v2", 60.0, 1).unwrap()
                - transcription_cost("scribe_v2", 60.0, 1).unwrap())
            .abs()
                < 1e-12
        );
    }
}
