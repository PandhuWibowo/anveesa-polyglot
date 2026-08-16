use crate::config::Config;
use crate::translate;
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct SheetOutcome {
    pub out_path: PathBuf,
    pub cells: usize,
    pub preview: Vec<(String, String)>,
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_spreadsheet(path: &Path) -> bool {
    matches!(ext_of(path).as_str(), "xlsx" | "xlsm" | "csv" | "tsv")
}

/// Translate a spreadsheet into `out_path`, preserving structure. For xlsx the
/// workbook is rewritten in place (formatting/formulas untouched); csv/tsv are
/// re-written cell by cell. `progress(done, total)` reports API batches.
pub fn translate_spreadsheet(
    cfg: &Config,
    path: &Path,
    out_path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<SheetOutcome> {
    match ext_of(path).as_str() {
        "xlsx" | "xlsm" => translate_xlsx(cfg, path, out_path, cancel, progress),
        "csv" | "tsv" => translate_csv(cfg, path, out_path, cancel, progress),
        other => bail!("not a supported spreadsheet: .{other}"),
    }
}

/// Cells worth sending to the API: anything containing a letter.
fn needs_translation(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic())
}

/// Unique translatable cell values in first-occurrence order, without
/// translating anything (used by Quick View).
pub fn list_texts(path: &Path) -> Result<Vec<String>> {
    let raw: Vec<String> = match ext_of(path).as_str() {
        "xlsx" | "xlsm" => {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            xlsx_text_parts(&bytes)?
                .into_iter()
                .flat_map(|p| p.texts)
                .collect()
        }
        "csv" | "tsv" => {
            let delimiter = if ext_of(path) == "tsv" { b'\t' } else { b',' };
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .delimiter(delimiter)
                .from_path(path)?;
            let mut cells = Vec::new();
            for record in reader.records() {
                for cell in record?.iter() {
                    cells.push(cell.to_string());
                }
            }
            cells
        }
        other => bail!("not a supported spreadsheet: .{other}"),
    };

    let mut seen = HashMap::new();
    let mut unique = Vec::new();
    for t in raw {
        if needs_translation(&t) && !seen.contains_key(&t) {
            seen.insert(t.clone(), ());
            unique.push(t);
        }
    }
    if unique.is_empty() {
        bail!("no translatable text found in this file");
    }
    Ok(unique)
}

/// Deduplicate, batch-translate, and return a original→translated map.
fn translate_unique(
    cfg: &Config,
    texts: &[String],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<HashMap<String, String>> {
    let mut unique: Vec<String> = Vec::new();
    let mut seen: HashMap<&str, ()> = HashMap::new();
    for t in texts {
        if needs_translation(t) && !seen.contains_key(t.as_str()) {
            seen.insert(t, ());
            unique.push(t.clone());
        }
    }

    // greedy batches: ≤40 cells and ≤2500 chars each
    let mut batches: Vec<&[String]> = Vec::new();
    let mut start = 0;
    while start < unique.len() {
        let mut end = start;
        let mut chars = 0;
        while end < unique.len() && end - start < 40 {
            chars += unique[end].chars().count();
            if chars > 2500 && end > start {
                break;
            }
            end += 1;
        }
        batches.push(&unique[start..end]);
        start = end;
    }

    let total = batches.len();
    progress(0, total);
    let mut map = HashMap::new();
    for (i, batch) in batches.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let translated = match translate::translate_batch(cfg, &cfg.model, batch) {
            Ok(t) => t,
            // model broke the numbered format — retry this batch cell by cell
            Err(_) => {
                let mut one_by_one = Vec::with_capacity(batch.len());
                for cell in *batch {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("cancelled");
                    }
                    one_by_one.push(translate::translate_cell(cfg, &cfg.model, cell)?);
                }
                one_by_one
            }
        };
        for (orig, tr) in batch.iter().zip(translated) {
            map.insert(orig.clone(), tr);
        }
        progress(i + 1, total);
    }
    Ok(map)
}

fn preview_of(map: &HashMap<String, String>) -> Vec<(String, String)> {
    map.iter()
        .filter(|(o, t)| o != t)
        .take(15)
        .map(|(o, t)| (o.clone(), t.clone()))
        .collect()
}

// ---------- xlsx ----------

/// Matches `<t>` text elements. In sharedStrings.xml they hold shared cell
/// text; in worksheet XML they hold inline strings (`<is><t>…</t></is>`) —
/// tool-generated workbooks (openpyxl, cloud exports) often have ONLY those.
fn t_regex() -> Regex {
    Regex::new(r"(?s)<t((?:\s[^>]*)?)>(.*?)</t>").unwrap()
}

struct TextPart {
    name: String,
    xml: String,
    texts: Vec<String>,
}

/// All zip entries that can contain cell text, with their `<t>` contents.
fn xlsx_text_parts(bytes: &[u8]) -> Result<Vec<TextPart>> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("opening xlsx (zip) archive")?;
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index_raw(i).ok().map(|f| f.name().to_string()))
        .filter(|n| {
            n == "xl/sharedStrings.xml"
                || (n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
        })
        .collect();

    let re = t_regex();
    let mut parts = Vec::new();
    for name in names {
        let mut xml = String::new();
        archive
            .by_name(&name)?
            .read_to_string(&mut xml)
            .with_context(|| format!("reading {name}"))?;
        let texts: Vec<String> = re.captures_iter(&xml).map(|c| xml_unescape(&c[2])).collect();
        if !texts.is_empty() {
            parts.push(TextPart { name, xml, texts });
        }
    }
    Ok(parts)
}

fn rewrite_part(part: &TextPart, map: &HashMap<String, String>) -> String {
    let mut idx = 0usize;
    t_regex()
        .replace_all(&part.xml, |caps: &regex::Captures<'_>| {
            let text = &part.texts[idx];
            idx += 1;
            let replacement = map.get(text).cloned().unwrap_or_else(|| text.clone());
            // preserve leading/trailing whitespace across round-trips
            let attrs = if caps[1].is_empty() && replacement.trim() != replacement {
                " xml:space=\"preserve\"".to_string()
            } else {
                caps[1].to_string()
            };
            format!("<t{attrs}>{}</t>", xml_escape(&replacement))
        })
        .into_owned()
}

fn translate_xlsx(
    cfg: &Config,
    path: &Path,
    out_path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<SheetOutcome> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let parts = xlsx_text_parts(&bytes)?;

    let all_texts: Vec<String> = parts.iter().flat_map(|p| p.texts.clone()).collect();
    if all_texts.iter().filter(|t| needs_translation(t)).count() == 0 {
        bail!("no translatable text found in this workbook");
    }

    let map = translate_unique(cfg, &all_texts, cancel, progress)?;

    let replacements: HashMap<String, String> = parts
        .iter()
        .map(|p| (p.name.clone(), rewrite_part(p, &map)))
        .collect();

    // copy every zip entry, swapping in the rewritten text-bearing ones
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes[..]))?;
    let out_file = std::fs::File::create(out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    let mut writer = zip::ZipWriter::new(out_file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..archive.len() {
        let name = archive.by_index_raw(i)?.name().to_string();
        if let Some(new_xml) = replacements.get(&name) {
            writer.start_file(&name, options)?;
            writer.write_all(new_xml.as_bytes())?;
        } else {
            let entry = archive.by_index_raw(i)?;
            writer.raw_copy_file(entry)?;
        }
    }
    writer.finish()?;

    Ok(SheetOutcome {
        out_path: out_path.to_path_buf(),
        cells: map.len(),
        preview: preview_of(&map),
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let decoded = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32);
                match decoded {
                    Some(c) => out.push(c),
                    None => out.push_str(&rest[..=end]), // unknown entity: keep as-is
                }
            }
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

// ---------- csv / tsv ----------

fn translate_csv(
    cfg: &Config,
    path: &Path,
    out_path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<SheetOutcome> {
    let delimiter = if ext_of(path) == "tsv" { b'\t' } else { b',' };
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_path(path)
        .with_context(|| format!("reading {}", path.display()))?;

    let records: Vec<csv::StringRecord> =
        reader.records().collect::<Result<_, _>>().context("parsing rows")?;
    let texts: Vec<String> = records
        .iter()
        .flat_map(|r| r.iter().map(|c| c.to_string()))
        .collect();
    if texts.iter().filter(|t| needs_translation(t)).count() == 0 {
        bail!("no translatable text found in this file");
    }

    let map = translate_unique(cfg, &texts, cancel, progress)?;

    let mut writer = csv::WriterBuilder::new()
        .flexible(true)
        .delimiter(delimiter)
        .from_path(out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    for record in &records {
        let row: Vec<&str> = record
            .iter()
            .map(|cell| map.get(cell).map(String::as_str).unwrap_or(cell))
            .collect();
        writer.write_record(&row)?;
    }
    writer.flush()?;

    Ok(SheetOutcome {
        out_path: out_path.to_path_buf(),
        cells: map.len(),
        preview: preview_of(&map),
    })
}
