use anyhow::{Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Stt {
    _ctx: WhisperContext,
    state: WhisperState,
    language: String,
}

impl Stt {
    /// Loads the Whisper model (slow — seconds). `language` is a Whisper code
    /// like "zh", or "auto" to detect per segment.
    pub fn load(model_path: &str, language: &str) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .with_context(|| format!("loading Whisper model from {model_path}"))?;
        let state = ctx.create_state().context("creating Whisper state")?;
        Ok(Self {
            _ctx: ctx,
            state,
            language: language.to_string(),
        })
    }

    /// Transcribe a chunk of 16 kHz mono f32 samples. Returns joined text.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if self.language != "auto" {
            params.set_language(Some(&self.language));
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
