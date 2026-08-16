use crate::config::Config;
use crate::translate;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

// Compiled by build.rs from pdfmask/main.m
const HELPER: &str = concat!(env!("OUT_DIR"), "/anveesa-pdfmask");

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PdfLine {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct Plan {
    lines: Vec<PdfLine>,
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}'
            | '\u{3040}'..='\u{30FF}' | '\u{AC00}'..='\u{D7AF}')
    })
}

pub fn list_lines(pdf: &Path) -> Result<Vec<PdfLine>> {
    let out = Command::new(HELPER)
        .arg("list")
        .arg(pdf)
        .output()
        .context("running the PDF mask helper")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let plan: Plan = serde_json::from_slice(&out.stdout).context("parsing PDF line list")?;
    Ok(plan.lines)
}

/// Translate every CJK line of `pdf` and write the translations back into the
/// same file as positioned overlay annotations. Returns lines masked.
pub fn translate_in_place(
    cfg: &Config,
    pdf: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<usize> {
    let lines = list_lines(pdf)?;
    let targets: Vec<PdfLine> = lines.into_iter().filter(|l| has_cjk(&l.text)).collect();
    if targets.is_empty() {
        bail!("no Chinese/Japanese/Korean text found in this PDF");
    }

    // dedupe repeated lines (headers, footers) before hitting the API
    let mut unique: Vec<String> = Vec::new();
    for l in &targets {
        if !unique.contains(&l.text) {
            unique.push(l.text.clone());
        }
    }

    const BATCH: usize = 40;
    let total = unique.len().div_ceil(BATCH);
    progress(0, total);
    let mut map: HashMap<String, String> = HashMap::new();
    for (i, chunk) in unique.chunks(BATCH).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let translated = match translate::translate_batch(cfg, &cfg.fast_model, chunk) {
            Ok(t) => t,
            Err(_) => {
                // batch format broke — retry this batch line by line
                let mut one = Vec::with_capacity(chunk.len());
                for cell in chunk {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("cancelled");
                    }
                    one.push(translate::translate_cell(cfg, &cfg.fast_model, cell)?);
                }
                one
            }
        };
        for (o, t) in chunk.iter().zip(translated) {
            map.insert(o.clone(), t);
        }
        progress(i + 1, total);
    }

    let masked: Vec<PdfLine> = targets
        .into_iter()
        .filter_map(|mut l| {
            let t = map.get(&l.text)?;
            l.text = t.clone();
            Some(l)
        })
        .collect();
    let count = masked.len();

    let plan_path = std::env::temp_dir().join("anveesa-pdfmask-plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&Plan { lines: masked })?)?;

    let out = Command::new(HELPER)
        .arg("apply")
        .arg(pdf)
        .arg(&plan_path)
        .output()
        .context("applying PDF annotations")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(count)
}

/// Path of the PDF currently frontmost in Preview, if any.
pub fn preview_front_document() -> Result<PathBuf> {
    let out = Command::new("osascript")
        .args(["-e", r#"tell application "Preview" to POSIX path of (path of front document)"#])
        .output()
        .context("asking Preview for its front document")?;
    if !out.status.success() {
        bail!("no document open in Preview? ({})",
            String::from_utf8_lossy(&out.stderr).trim());
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("pdf")) != Some(true) {
        bail!("the front Preview document is not a PDF: {}", path.display());
    }
    Ok(path)
}

/// Make Preview re-read the file from disk so the annotations show up.
pub fn reload_in_preview(pdf: &Path) {
    let script = format!(
        r#"tell application "Preview"
  close (every document whose path is "{p}")
  open POSIX file "{p}"
  activate
end tell"#,
        p = pdf.display()
    );
    let _ = Command::new("osascript").args(["-e", &script]).output();
}
