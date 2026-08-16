use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// OpenAI-compatible API base, e.g. https://ai.sumopod.com/v1
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    /// Fast model for on-the-fly Quick View translations
    pub fast_model: String,
    /// Language to translate INTO, written in plain words ("English", "Indonesian", …)
    pub target_lang: String,
    /// Seconds between screen captures
    pub interval_secs: f32,
    /// Skip OCR lines below this confidence (0.0 – 1.0)
    pub min_confidence: f64,
    /// Truncate OCR text sent to the API at this many characters
    pub max_chars: usize,
    /// Path to the Whisper GGML model used for live captions
    pub whisper_model: String,
    /// Spoken-language hint for Whisper ("zh", "auto", …)
    pub stt_lang: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_base: "https://ai.sumopod.com/v1".into(),
            api_key: String::new(),
            model: "deepseek-v4-pro".into(),
            fast_model: "gemini/gemini-3.5-flash-lite".into(),
            target_lang: "Indonesian".into(),
            interval_secs: 3.0,
            min_confidence: 0.3,
            max_chars: 6000,
            whisper_model: "models/ggml-large-v3-turbo-q5_0.bin".into(),
            stt_lang: "zh".into(),
        }
    }
}

fn config_path() -> PathBuf {
    // Prefer a config.toml next to where the app is run from; fall back to
    // ~/.config/anveesa-polyglot/config.toml
    let local = PathBuf::from("config.toml");
    if local.exists() {
        return local;
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("anveesa-polyglot")
        .join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
