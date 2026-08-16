use anyhow::{Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Stt {
    _ctx: WhisperContext,
    state: WhisperState,
    /// Some(code) pins every segment to one language (best accuracy for a
    /// single known speaker language); None lets Whisper detect the spoken
    /// language independently for each segment — the mechanism that makes
    /// "more than one language" work, since a fixed hint can only ever be one
    /// code. There's no cap: whichever of Whisper's ~99 languages is spoken
    /// in a given utterance is what gets detected, meeting to meeting.
    language: Option<String>,
}

impl Stt {
    /// Loads the Whisper model (slow — seconds). `spoken_langs` is a
    /// comma-separated list of Whisper codes (e.g. "zh,ja,ko"), or "auto".
    /// Exactly one code pins recognition to that language; zero, "auto", or
    /// several fall back to per-segment auto-detection (see `language`).
    pub fn load(model_path: &str, spoken_langs: &str) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .with_context(|| format!("loading Whisper model from {model_path}"))?;
        let state = ctx.create_state().context("creating Whisper state")?;
        Ok(Self {
            _ctx: ctx,
            state,
            language: single_language_hint(spoken_langs),
        })
    }

    /// Transcribe a chunk of 16 kHz mono f32 samples. Returns joined text.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if let Some(lang) = &self.language {
            params.set_language(Some(lang));
        }
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_no_context(true);

        self.state
            .full(params, samples)
            .context("running Whisper transcription")?;

        let n = self.state.full_n_segments();
        let mut text = String::new();
        for seg in 0..n {
            if let Some(segment) = self.state.get_segment(seg) {
                if let Ok(s) = segment.to_str() {
                    text.push_str(s);
                }
            }
        }
        Ok(clean_transcript(&text))
    }
}

/// Common Whisper language codes for the picker UI. Whisper supports ~99
/// languages total (ISO 639-1); the settings text field accepts any of them
/// even if not listed here — this is just the quick-pick shortlist.
pub const COMMON_LANGUAGES: &[(&str, &str)] = &[
    ("zh", "Mandarin"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("en", "English"),
    ("id", "Indonesian"),
    ("ms", "Malay"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("hi", "Hindi"),
    ("ar", "Arabic"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("it", "Italian"),
    ("nl", "Dutch"),
    ("tr", "Turkish"),
];

fn single_language_hint(spoken_langs: &str) -> Option<String> {
    let codes: Vec<&str> = spoken_langs
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty() && *c != "auto")
        .collect();
    match codes.as_slice() {
        [one] => Some((*one).to_string()),
        _ => None, // 0 or 2+ codes: auto-detect per segment
    }
}

/// Whisper emits sound-event markers like (音乐) or [BLANK_AUDIO] on
/// non-speech audio; strip them so they never reach the caption list.
fn clean_transcript(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            '(' | '[' | '（' | '【' => depth += 1,
            ')' | ']' | '）' | '】' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::single_language_hint;

    #[test]
    fn zero_or_auto_means_autodetect() {
        assert_eq!(single_language_hint(""), None);
        assert_eq!(single_language_hint("auto"), None);
    }

    #[test]
    fn exactly_one_pins_the_language() {
        assert_eq!(single_language_hint("zh"), Some("zh".into()));
        assert_eq!(single_language_hint(" zh , auto "), Some("zh".into()));
    }

    #[test]
    fn two_or_more_means_autodetect() {
        assert_eq!(single_language_hint("zh,ja"), None);
        assert_eq!(single_language_hint("zh,ja,ko"), None);
    }
}
