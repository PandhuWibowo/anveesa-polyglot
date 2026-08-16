use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Streaming translation: `on_delta` receives translation text as it arrives.
/// Reasoning deltas are skipped — only real content is surfaced.
pub fn stream_translate(
    cfg: &Config,
    model: &str,
    system: &str,
    text: &str,
    cancel: &std::sync::atomic::AtomicBool,
    on_delta: &mut dyn FnMut(&str),
) -> Result<String> {
    use std::io::BufRead;

    let mut body = base_body(cfg, model, system, text);
    body["stream"] = json!(true);
    let url = format!("{}/chat/completions", cfg.api_base.trim_end_matches('/'));
    let mut response = agent()
        .post(&url)
        .header("Authorization", &format!("Bearer {}", cfg.api_key))
        .send_json(&body)
        .context("calling translation API (stream)")?;

    let reader = std::io::BufReader::new(response.body_mut().as_reader());
    let mut full = String::new();
    for line in reader.lines() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let line = line.context("reading stream")?;
        let Some(data) = line.strip_prefix("data: ") else { continue };
        if data.trim() == "[DONE]" {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(err) = value.get("error") {
            return Err(anyhow!("API error: {err}"));
        }
        if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
            if !delta.is_empty() {
                full.push_str(delta);
                on_delta(delta);
            }
        }
    }
    Ok(full.trim().to_string())
}

/// System prompt for document translation (shared by chunked + streamed paths).
pub fn doc_system_prompt(target_lang: &str) -> String {
    format!(
        "You are a document translator. The user message is a portion of a larger \
         document (it may start or end mid-section). Translate all natural-language \
         text into {target_lang}. Preserve the structure exactly: Markdown/markup \
         syntax, indentation, tables, lists, blank lines. In code, translate only \
         comments and human-readable strings — never identifiers, keywords, or \
         values. Leave proper nouns, numbers, and URLs as-is. Output ONLY the \
         translated content, with no commentary."
    )
}

/// Translate independent spreadsheet cells in one call, using a numbered-line
/// protocol (`N: value`, `\n` escaped). Errors if the model breaks the format.
pub fn translate_batch(cfg: &Config, model: &str, cells: &[String]) -> Result<Vec<String>> {
    let system = format!(
        "You are a spreadsheet translator. The user message is a numbered list of \
         independent spreadsheet cell values, one per line, in the form `N: value` \
         (a literal \\n inside a value stands for a line break). Translate each value \
         into {lang}. Return values that are codes, IDs, part numbers, dates, or \
         other non-language content unchanged. Reply with EXACTLY the same numbered \
         format: every number exactly once, one line each, nothing else.",
        lang = cfg.target_lang
    );
    let payload = cells
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}: {}", i + 1, escape_cell(c)))
        .collect::<Vec<_>>()
        .join("\n");
    let response = chat_with(cfg, model, &system, &payload)?;
    parse_batch(&response, cells.len())
}

/// Fallback: translate a single cell value.
pub fn translate_cell(cfg: &Config, model: &str, text: &str) -> Result<String> {
    let system = format!(
        "Translate this spreadsheet cell value into {lang}. If it is a code, ID, \
         date, or other non-language content, return it unchanged. Output ONLY the \
         resulting value.",
        lang = cfg.target_lang
    );
    chat_with(cfg, model, &system, text)
}

fn escape_cell(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\r', "").replace('\n', "\\n")
}

fn unescape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_batch(response: &str, n: usize) -> Result<Vec<String>> {
    let mut out: Vec<Option<String>> = vec![None; n];
    for line in response.lines() {
        let Some((num, rest)) = line.split_once(':') else { continue };
        let Ok(idx) = num.trim().trim_start_matches('`').parse::<usize>() else {
            continue;
        };
        if (1..=n).contains(&idx) {
            out[idx - 1] = Some(unescape_cell(rest.strip_prefix(' ').unwrap_or(rest)));
        }
    }
    let missing = out.iter().filter(|o| o.is_none()).count();
    if missing > 0 {
        return Err(anyhow!("batch reply missing {missing} of {n} entries"));
    }
    Ok(out.into_iter().flatten().collect())
}

/// Translate one chunk of a document into `cfg.target_lang`. Reasoning
/// models can be unreliable on unusual content (e.g. dense archaic/classical
/// text) — very slow, or "successfully" returning nothing because reasoning
/// ate the whole token budget. If the quality model fails, retry once with
/// the fast model rather than losing the whole chunk.
pub fn translate_document(cfg: &Config, text: &str) -> Result<String> {
    let system = doc_system_prompt(&cfg.target_lang);
    match chat_with(cfg, &cfg.model, &system, text) {
        Ok(t) => Ok(t),
        Err(primary_err) if cfg.fast_model != cfg.model => {
            chat_with(cfg, &cfg.fast_model, &system, text)
                .map_err(|_| primary_err.context("fast-model fallback also failed"))
        }
        Err(e) => Err(e),
    }
}

/// Translate a spoken caption into `cfg.target_lang`.
pub fn translate_caption(cfg: &Config, text: &str) -> Result<String> {
    let system = format!(
        "You are a live-captions translator. The user message is one utterance \
         transcribed from speech (it may have transcription errors). Translate it \
         into {lang}, naturally and briefly, fixing obvious mis-transcriptions from \
         context. Output ONLY the translation.",
        lang = cfg.target_lang
    );
    chat(cfg, &system, text)
}

/// Translate `text` into `cfg.target_lang` via the OpenAI-compatible API.
pub fn translate(cfg: &Config, text: &str) -> Result<String> {
    let system = format!(
        "You are a real-time translation engine. The user message is text captured \
         from a computer screen by OCR, so it may contain UI fragments, menus, or \
         noise mixed with the meaningful content. Translate everything meaningful \
         into {lang}. Keep the original line structure so the reader can match the \
         translation to the screen. Leave proper nouns, code, numbers and URLs as-is. \
         If a line is already in {lang}, keep it unchanged. Output ONLY the \
         translation — no commentary, no explanations.",
        lang = cfg.target_lang
    );
    chat(cfg, &system, text)
}

fn chat(cfg: &Config, system: &str, text: &str) -> Result<String> {
    chat_with(cfg, &cfg.model, system, text)
}

fn agent() -> ureq::Agent {
    // reasoning models can take a while, but a hung connection must not
    // wedge a job forever
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(180)))
        .build()
        .into()
}

/// When the fast model is used, ask reasoning models to skip deliberation —
/// speed is the whole point of that path (ignored by non-reasoning models).
fn base_body(cfg: &Config, model: &str, system: &str, text: &str) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": text},
        ],
        "temperature": 0.2,
        "max_tokens": 8000,
    });
    if model == cfg.fast_model {
        body["reasoning_effort"] = json!("minimal");
        body["max_tokens"] = json!(3000);
    }
    body
}

pub fn chat_with(cfg: &Config, model: &str, system: &str, text: &str) -> Result<String> {
    let body = base_body(cfg, model, system, text);

    let url = format!("{}/chat/completions", cfg.api_base.trim_end_matches('/'));
    let mut response = agent()
        .post(&url)
        .header("Authorization", &format!("Bearer {}", cfg.api_key))
        .send_json(&body)
        .context("calling translation API")?;

    let value: Value = response
        .body_mut()
        .read_json()
        .context("parsing translation API response")?;

    if let Some(err) = value.get("error") {
        return Err(anyhow!("API error: {err}"));
    }

    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| anyhow!("unexpected API response shape: {value}"))?;

    if content.is_empty() && !text.trim().is_empty() {
        // Reasoning models can burn the whole token budget "thinking" and
        // leave nothing for the actual answer (finish_reason "length" with
        // empty content) — that's a failure, not a valid empty translation.
        let finish = value["choices"][0]["finish_reason"].as_str().unwrap_or("?");
        bail!("model \"{model}\" returned no content (finish_reason: {finish})");
    }
    Ok(content)
}
