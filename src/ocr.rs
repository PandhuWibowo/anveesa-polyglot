use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;
use xcap::image::RgbaImage;

// Compiled by build.rs from ocr/main.m
const OCR_HELPER: &str = concat!(env!("OUT_DIR"), "/anveesa-ocr");

#[derive(Debug, Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub confidence: f64,
    /// Normalized bounding box [x, y, width, height], bottom-left origin
    /// (Vision's convention) — fractions of the captured frame, 0.0–1.0.
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
}

#[derive(Debug, Deserialize)]
struct OcrOutput {
    lines: Vec<OcrLine>,
}

fn temp_image_path() -> PathBuf {
    std::env::temp_dir().join("anveesa-polyglot-frame.png")
}

/// Run Apple Vision OCR on a captured frame and return recognized lines.
pub fn recognize(image: &RgbaImage) -> Result<Vec<OcrLine>> {
    let path = temp_image_path();
    image.save(&path).context("saving frame for OCR")?;

    let output = Command::new(OCR_HELPER)
        .arg(&path)
        .output()
        .context("running the Vision OCR helper")?;

    if !output.status.success() {
        bail!(
            "OCR helper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let parsed: OcrOutput =
        serde_json::from_slice(&output.stdout).context("parsing OCR helper output")?;
    Ok(parsed.lines)
}

/// Join OCR lines into the text block we send for translation.
pub fn lines_to_text(lines: &[OcrLine], min_confidence: f64, max_chars: usize) -> String {
    let mut text = String::new();
    for line in lines {
        if line.confidence < min_confidence {
            continue;
        }
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(trimmed);
        if text.chars().count() >= max_chars {
            break;
        }
    }
    text.chars().take(max_chars).collect()
}
