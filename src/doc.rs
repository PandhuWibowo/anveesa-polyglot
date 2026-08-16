use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

// Compiled by build.rs from doc/main.m
const DOC_HELPER: &str = concat!(env!("OUT_DIR"), "/anveesa-doc");

/// Formats macOS `textutil` can convert to plain text.
const TEXTUTIL_EXTS: &[&str] = &["docx", "doc", "rtf", "rtfd", "html", "htm", "odt", "webarchive"];

/// Extract readable text from a file: PDFs via PDFKit, office/rich-text
/// formats via `textutil`, anything else read as (lossy) UTF-8.
pub fn extract_text(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    let text = if ext == "pdf" {
        let out = Command::new(DOC_HELPER)
            .arg(path)
            .output()
            .context("running the PDF helper")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else if TEXTUTIL_EXTS.contains(&ext.as_str()) {
        let out = Command::new("textutil")
            .args(["-convert", "txt", "-stdout"])
            .arg(path)
            .output()
            .context("running textutil")?;
        if !out.status.success() {
            bail!("textutil failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.iter().take(1024).filter(|&&b| b == 0).count() > 4 {
            bail!("this looks like a binary file, not text");
        }
        String::from_utf8_lossy(&bytes).into_owned()
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("no text found in {}", path.display());
    }
    Ok(text)
}

/// Split text into chunks of at most `max_chars`, preferring paragraph
/// boundaries, then line boundaries, then a hard character split.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    let push_piece = |piece: &str, chunks: &mut Vec<String>, current: &mut String| {
        if !current.is_empty() && current.chars().count() + piece.chars().count() + 2 > max_chars {
            chunks.push(std::mem::take(current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(piece);
    };

    for para in text.split("\n\n") {
        if para.chars().count() <= max_chars {
            push_piece(para, &mut chunks, &mut current);
            continue;
        }
        // oversized paragraph: split by lines, hard-split any giant line
        for line in para.lines() {
            let mut rest: &str = line;
            while rest.chars().count() > max_chars {
                let cut = rest
                    .char_indices()
                    .nth(max_chars)
                    .map(|(i, _)| i)
                    .unwrap_or(rest.len());
                push_piece(&rest[..cut], &mut chunks, &mut current);
                rest = &rest[cut..];
            }
            push_piece(rest, &mut chunks, &mut current);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::chunk_text;

    #[test]
    fn small_text_is_one_chunk() {
        assert_eq!(chunk_text("hello\n\nworld", 100), vec!["hello\n\nworld"]);
    }

    #[test]
    fn splits_on_paragraphs() {
        let a = "a".repeat(2000);
        let b = "b".repeat(2000);
        let c = "c".repeat(2000);
        let text = format!("{a}\n\n{b}\n\n{c}");
        let chunks = chunk_text(&text, 3000);
        assert_eq!(chunks, vec![a, b, c]);
    }

    #[test]
    fn hard_splits_giant_lines() {
        let text = "x".repeat(7000);
        let chunks = chunk_text(&text, 3000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.join("").replace("\n\n", ""), text);
    }

    #[test]
    fn multibyte_chars_split_on_boundaries() {
        let text = "汉".repeat(5000);
        let chunks = chunk_text(&text, 3000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.join("").replace("\n\n", ""), text);
    }
}
