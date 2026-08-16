use crate::capture::{self, CaptureTarget};
use crate::captions::{Caption, CaptionEngine};
use crate::config::Config;
use crate::{doc, mask, numbers, ocr, pdfmask, sheet, stt, theme, translate};
use eframe::egui;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Untranslated remainder of a Quick View session, translated on demand.
pub enum QuickSource {
    Doc { chunks: Vec<String>, next: usize },
    Sheet { texts: Vec<String>, next: usize },
}

/// Cell values per Quick View batch for spreadsheets.
const QUICK_SHEET_BATCH: usize = 40;
/// Chars per Quick View chunk for documents (small = fast first result).
const QUICK_DOC_CHUNK: usize = 1500;

pub struct NumbersJob {
    pub doc_name: String,
    pub sheet_name: String,
    pub done: usize,
    pub total: usize,
    pub running: bool,
    pub error: Option<String>,
}

pub struct PdfJob {
    pub doc_name: String,
    pub done: usize,
    pub total: usize,
    pub masked: usize,
    pub running: bool,
    pub error: Option<String>,
}

pub struct FileJob {
    pub path: PathBuf,
    pub done: usize,
    pub total: usize,
    pub result: String,
    pub error: Option<String>,
    pub running: bool,
    pub saved_to: Option<PathBuf>,
    pub quick: Option<QuickSource>,
    /// True for a right-click "Translate Selection" job — there's no source
    /// file, so the window hides "Translate all & save".
    pub is_selection: bool,
}

pub struct Shared {
    pub cfg: Config,
    pub targets: Vec<CaptureTarget>,
    pub selected: Option<CaptureTarget>,
    pub paused: bool,
    pub original: String,
    pub translation: String,
    pub status: String,
    pub last_update: Option<Instant>,
    pub captions: Vec<Caption>,
    pub caption_status: String,
    pub file_job: Option<FileJob>,
    pub pending_files: Vec<PathBuf>,
    pub numbers_job: Option<NumbersJob>,
    pub pdf_job: Option<PdfJob>,
    /// Cancel flag for the current file job (UI button flips it).
    pub file_cancel: Arc<AtomicBool>,
    /// Set by whoever starts a job; the UI consumes it to open the window.
    pub file_window_requested: bool,
    pub mask_lines: Vec<crate::mask::MaskLine>,
    pub mask_target_rect: Option<(f32, f32, f32, f32)>,
    /// The actual captured frame, painted as the overlay's background (see
    /// mask.rs — this replaced relying on real window transparency).
    pub mask_texture: Option<egui::TextureHandle>,
}

/// Directory watched for queued file paths (written by the Finder Quick
/// Action): each `*.path` file contains one absolute path to translate.
pub fn queue_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("anveesa-polyglot")
        .join("queue")
}

pub struct App {
    shared: Arc<Mutex<Shared>>,
    show_original: bool,
    show_settings: bool,
    engine: Option<CaptionEngine>,
    mask_engine: Option<mask::MaskEngine>,
    show_file_window: bool,
    custom_lang_input: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::theme::apply(&cc.egui_ctx);
        install_cjk_font(&cc.egui_ctx);

        let cfg = Config::load().unwrap_or_default();
        let status = if cfg.api_key.is_empty() {
            "⚠ No API key — open Settings below".to_string()
        } else {
            "Pick a window or screen to translate".to_string()
        };

        let shared = Arc::new(Mutex::new(Shared {
            cfg,
            targets: capture::list_targets(),
            selected: None,
            paused: false,
            original: String::new(),
            translation: String::new(),
            status,
            last_update: None,
            captions: Vec::new(),
            caption_status: String::new(),
            file_job: None,
            pending_files: Vec::new(),
            numbers_job: None,
            pdf_job: None,
            file_cancel: Arc::new(AtomicBool::new(false)),
            file_window_requested: false,
            mask_lines: Vec::new(),
            mask_target_rect: None,
            mask_texture: None,
        }));

        spawn_worker(shared.clone(), cc.egui_ctx.clone());
        spawn_queue_watcher(shared.clone(), cc.egui_ctx.clone());

        Self {
            shared,
            show_original: false,
            show_settings: false,
            engine: None,
            mask_engine: None,
            show_file_window: false,
            custom_lang_input: String::new(),
        }
    }
}

/// Start a Quick View for `path` if no job is running. Callable from any thread.
fn try_start_file_job(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, path: PathBuf) {
    let cancel = {
        let mut s = shared.lock().unwrap();
        if s.file_job.as_ref().is_some_and(|j| j.running) {
            return; // one job at a time
        }
        // claim the slot synchronously so two starters can't race
        s.file_job = Some(FileJob {
            path: path.clone(),
            done: 0,
            total: 0,
            result: String::new(),
            error: None,
            running: true,
            saved_to: None,
            quick: None,
            is_selection: false,
        });
        s.file_cancel = Arc::new(AtomicBool::new(false));
        s.file_window_requested = true;
        s.file_cancel.clone()
    };

    let shared = shared.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        eprintln!("[quick] start {}", path.display());
        let prepared = if sheet::is_spreadsheet(&path) {
            sheet::list_texts(&path).map(|texts| QuickSource::Sheet { texts, next: 0 })
        } else {
            doc::extract_text(&path)
                .map(|t| doc::chunk_text(&t, QUICK_DOC_CHUNK))
                .map(|chunks| QuickSource::Doc { chunks, next: 0 })
        };
        match prepared {
            Ok(quick) => {
                let total = match &quick {
                    QuickSource::Doc { chunks, .. } => chunks.len(),
                    QuickSource::Sheet { texts, .. } => texts.len().div_ceil(QUICK_SHEET_BATCH),
                };
                {
                    let mut s = shared.lock().unwrap();
                    if let Some(job) = &mut s.file_job {
                        job.total = total;
                        job.quick = Some(quick);
                    }
                }
                quick_step(&shared, &ctx, &cancel);
            }
            Err(e) => {
                finish(&shared, Some(format!("⚠ {e:#}")));
                ctx.request_repaint();
            }
        }
    });
}

/// Start a Quick View for raw text selected in another app (right-click →
/// Services → "Translate Selection with Polyglot"). Single chunk, streams
/// immediately — no file to read, no file to save.
fn try_start_text_job(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, text: String) {
    let cancel = {
        let mut s = shared.lock().unwrap();
        if s.file_job.as_ref().is_some_and(|j| j.running) {
            return; // one job at a time
        }
        let chunks = doc::chunk_text(&text, QUICK_DOC_CHUNK);
        s.file_job = Some(FileJob {
            path: PathBuf::from("Selected text"),
            done: 0,
            total: chunks.len(),
            result: String::new(),
            error: None,
            running: true,
            saved_to: None,
            quick: Some(QuickSource::Doc { chunks, next: 0 }),
            is_selection: true,
        });
        s.file_cancel = Arc::new(AtomicBool::new(false));
        s.file_window_requested = true;
        s.file_cancel.clone()
    };
    let shared = shared.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || quick_step(&shared, &ctx, &cancel));
}

/// Translate the next Quick View part (streaming for documents, one batch of
/// cell pairs for spreadsheets). Expects `job.running == true` on entry.
fn quick_step(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, cancel: &Arc<AtomicBool>) {
    enum Task {
        Doc(String),
        Sheet(Vec<String>),
    }
    let (cfg, task) = {
        let mut s = shared.lock().unwrap();
        let cfg = s.cfg.clone();
        let Some(job) = &mut s.file_job else { return };
        let task = match &job.quick {
            Some(QuickSource::Doc { chunks, next }) if *next < chunks.len() => {
                Task::Doc(chunks[*next].clone())
            }
            Some(QuickSource::Sheet { texts, next }) if next * QUICK_SHEET_BATCH < texts.len() => {
                let start = next * QUICK_SHEET_BATCH;
                let end = (start + QUICK_SHEET_BATCH).min(texts.len());
                Task::Sheet(texts[start..end].to_vec())
            }
            _ => {
                job.running = false;
                return;
            }
        };
        if !job.result.is_empty() {
            job.result.push_str("\n\n");
        }
        (cfg, task)
    };
    ctx.request_repaint();

    let outcome = match task {
        Task::Doc(chunk) => {
            let system = translate::doc_system_prompt(&cfg.target_lang);
            let shared_cb = shared.clone();
            let ctx_cb = ctx.clone();
            translate::stream_translate(&cfg, &cfg.fast_model, &system, &chunk, cancel, &mut |d| {
                let mut s = shared_cb.lock().unwrap();
                if let Some(job) = &mut s.file_job {
                    job.result.push_str(d);
                }
                drop(s);
                ctx_cb.request_repaint();
            })
            .map(|_| ())
        }
        Task::Sheet(cells) => {
            translate::translate_batch(&cfg, &cfg.fast_model, &cells).map(|translated| {
                let mut s = shared.lock().unwrap();
                if let Some(job) = &mut s.file_job {
                    for (o, t) in cells.iter().zip(&translated) {
                        job.result.push_str(&format!("{o}\n    → {t}\n"));
                    }
                }
            })
        }
    };

    let mut s = shared.lock().unwrap();
    if let Some(job) = &mut s.file_job {
        match outcome {
            Ok(()) => {
                eprintln!("[quick] part {} done ({} chars shown)", job.done + 1, job.result.chars().count());
                job.done += 1;
                match &mut job.quick {
                    Some(QuickSource::Doc { next, .. }) | Some(QuickSource::Sheet { next, .. }) => {
                        *next += 1;
                    }
                    None => {}
                }
                job.error = None;
            }
            Err(e) => job.error = Some(format!("⚠ {e:#}")),
        }
        job.running = false;
    }
    drop(s);
    ctx.request_repaint();
}

/// "Continue" button: translate the next part in a background thread.
fn continue_quick(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    let cancel = {
        let mut s = shared.lock().unwrap();
        let Some(job) = &mut s.file_job else { return };
        if job.running {
            return;
        }
        job.running = true;
        s.file_cancel = Arc::new(AtomicBool::new(false));
        s.file_cancel.clone()
    };
    let shared = shared.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || quick_step(&shared, &ctx, &cancel));
}

/// "Translate all & save": full-quality translation of the whole file,
/// written next to the original (real workbook for spreadsheets).
fn translate_all_and_save(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    let (path, cancel) = {
        let mut s = shared.lock().unwrap();
        let Some(job) = &mut s.file_job else { return };
        if job.running {
            return;
        }
        job.running = true;
        job.done = 0;
        s.file_cancel = Arc::new(AtomicBool::new(false));
        (
            s.file_job.as_ref().unwrap().path.clone(),
            s.file_cancel.clone(),
        )
    };
    let shared = shared.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let cfg = shared.lock().unwrap().cfg.clone();
        eprintln!("[full] start {}", path.display());

        if sheet::is_spreadsheet(&path) {
            let out = spreadsheet_out_path(&path, &cfg.target_lang);
            let mut progress = |done: usize, total: usize| {
                let mut s = shared.lock().unwrap();
                if let Some(job) = &mut s.file_job {
                    job.done = done;
                    job.total = total;
                }
                drop(s);
                ctx.request_repaint();
            };
            match sheet::translate_spreadsheet(&cfg, &path, &out, &cancel, &mut progress) {
                Ok(outcome) => {
                    let mut s = shared.lock().unwrap();
                    if let Some(job) = &mut s.file_job {
                        job.result = format!(
                            "Translated {} unique cell value(s).\n\nSaved to:\n{}",
                            outcome.cells,
                            outcome.out_path.display()
                        );
                        job.saved_to = Some(outcome.out_path);
                        job.quick = None;
                        job.running = false;
                        job.error = None;
                    }
                }
                Err(e) => finish(&shared, Some(format!("⚠ {e:#}"))),
            }
            ctx.request_repaint();
            return;
        }

        // document: translate remaining chunks at full quality, then save
        let (mut result, chunks, start) = {
            let s = shared.lock().unwrap();
            match s.file_job.as_ref().and_then(|j| j.quick.as_ref()) {
                Some(QuickSource::Doc { chunks, next }) => {
                    let job = s.file_job.as_ref().unwrap();
                    (job.result.clone(), chunks.clone(), *next)
                }
                _ => {
                    drop(s);
                    finish(&shared, Some("⚠ nothing to translate".into()));
                    return;
                }
            }
        };
        {
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.file_job {
                job.total = chunks.len();
                job.done = start;
            }
        }
        for (i, chunk) in chunks.iter().enumerate().skip(start) {
            if cancel.load(Ordering::Relaxed) {
                finish(&shared, Some("cancelled".into()));
                ctx.request_repaint();
                return;
            }
            match translate::translate_document(&cfg, chunk) {
                Ok(t) => {
                    if !result.is_empty() {
                        result.push_str("\n\n");
                    }
                    result.push_str(&t);
                    let mut s = shared.lock().unwrap();
                    if let Some(job) = &mut s.file_job {
                        job.result = result.clone();
                        job.done = i + 1;
                        if let Some(QuickSource::Doc { next, .. }) = &mut job.quick {
                            *next = i + 1;
                        }
                    }
                    ctx.request_repaint();
                }
                Err(e) => {
                    finish(&shared, Some(format!("⚠ part {}: {e:#}", i + 1)));
                    ctx.request_repaint();
                    return;
                }
            }
        }
        let out = translated_path(&path, &cfg.target_lang);
        {
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.file_job {
                match std::fs::write(&out, &job.result) {
                    Ok(()) => {
                        eprintln!("[full] saved {}", out.display());
                        job.saved_to = Some(out);
                        job.error = None;
                    }
                    Err(e) => job.error = Some(format!("⚠ save failed: {e}")),
                }
                job.running = false;
            }
        }
        ctx.request_repaint();
    });
}

fn finish(shared: &Arc<Mutex<Shared>>, error: Option<String>) {
    let mut s = shared.lock().unwrap();
    if let Some(job) = &mut s.file_job {
        job.running = false;
        job.error = error;
    }
}

/// Translate the sheet currently open (and active) in Numbers, in place:
/// every text cell becomes "original\ntranslation", written directly back
/// into the live document via AppleScript. Nothing is saved to disk unless
/// the user saves in Numbers themselves.
fn start_numbers_job(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    {
        let mut s = shared.lock().unwrap();
        if s.numbers_job.as_ref().is_some_and(|j| j.running) {
            return;
        }
        s.numbers_job = Some(NumbersJob {
            doc_name: String::new(),
            sheet_name: String::new(),
            done: 0,
            total: 0,
            running: true,
            error: None,
        });
    }
    let shared = shared.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let cfg = shared.lock().unwrap().cfg.clone();

        let info = match numbers::active_sheet_info() {
            Ok(info) => info,
            Err(e) => {
                numbers_finish(&shared, &ctx, Some(format!("⚠ {e:#}")));
                return;
            }
        };
        {
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.numbers_job {
                job.doc_name = info.doc_name.clone();
                job.sheet_name = info.sheet_name.clone();
            }
        }
        ctx.request_repaint();

        let cells = match numbers::read_translatable_cells(&info) {
            Ok(cells) => cells,
            Err(e) => {
                numbers_finish(&shared, &ctx, Some(format!("⚠ {e:#}")));
                return;
            }
        };
        // skip cells that already look stacked (contain a soft-return line
        // break) so clicking the button twice doesn't double-translate
        let cells: Vec<_> = cells.into_iter().filter(|c| !c.text.contains('\r')).collect();

        let mut unique: Vec<String> = Vec::new();
        for c in &cells {
            if !unique.contains(&c.text) {
                unique.push(c.text.clone());
            }
        }
        {
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.numbers_job {
                job.total = unique.len();
            }
        }
        ctx.request_repaint();

        const BATCH: usize = 40;
        let mut translated: std::collections::HashMap<String, String> = Default::default();
        for chunk in unique.chunks(BATCH) {
            match translate::translate_batch(&cfg, &cfg.fast_model, chunk) {
                Ok(out) => {
                    for (o, t) in chunk.iter().zip(out) {
                        translated.insert(o.clone(), t);
                    }
                }
                Err(e) => {
                    numbers_finish(&shared, &ctx, Some(format!("⚠ translation failed: {e:#}")));
                    return;
                }
            }
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.numbers_job {
                job.done = translated.len();
            }
            drop(s);
            ctx.request_repaint();
        }

        let writes: Vec<(String, String, String)> = cells
            .into_iter()
            .filter_map(|c| translated.get(&c.text).map(|t| (c.cell_ref, c.text, t.clone())))
            .collect();
        // Numbers automation gets slower per statement as scripts grow long;
        // write in modest batches so one huge sheet doesn't time out.
        for chunk in writes.chunks(150) {
            if let Err(e) = numbers::write_stacked_cells(chunk) {
                numbers_finish(&shared, &ctx, Some(format!("⚠ writing to Numbers failed: {e:#}")));
                return;
            }
        }

        numbers_finish(&shared, &ctx, None);
    });
}

/// Translate the PDF frontmost in Preview, in place, and reload it there.
fn start_pdf_job(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    {
        let mut s = shared.lock().unwrap();
        if s.pdf_job.as_ref().is_some_and(|j| j.running) {
            return;
        }
        s.pdf_job = Some(PdfJob {
            doc_name: String::new(),
            done: 0,
            total: 0,
            masked: 0,
            running: true,
            error: None,
        });
    }
    let shared = shared.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let cfg = shared.lock().unwrap().cfg.clone();
        let path = match pdfmask::preview_front_document() {
            Ok(p) => p,
            Err(e) => {
                pdf_finish(&shared, &ctx, 0, Some(format!("⚠ {e:#}")));
                return;
            }
        };
        eprintln!("[pdf] start {}", path.display());
        {
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.pdf_job {
                job.doc_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
            }
        }
        ctx.request_repaint();

        let cancel = AtomicBool::new(false);
        let mut progress = |done: usize, total: usize| {
            let mut s = shared.lock().unwrap();
            if let Some(job) = &mut s.pdf_job {
                job.done = done;
                job.total = total;
            }
            drop(s);
            ctx.request_repaint();
        };
        match pdfmask::translate_in_place(&cfg, &path, &cancel, &mut progress) {
            Ok(masked) => {
                eprintln!("[pdf] masked {masked} line(s), reloading Preview");
                pdfmask::reload_in_preview(&path);
                pdf_finish(&shared, &ctx, masked, None);
            }
            Err(e) => pdf_finish(&shared, &ctx, 0, Some(format!("⚠ {e:#}"))),
        }
    });
}

fn pdf_finish(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, masked: usize, error: Option<String>) {
    let mut s = shared.lock().unwrap();
    if let Some(job) = &mut s.pdf_job {
        job.running = false;
        job.masked = masked;
        job.error = error;
    }
    drop(s);
    ctx.request_repaint();
}

fn numbers_finish(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, error: Option<String>) {
    let mut s = shared.lock().unwrap();
    if let Some(job) = &mut s.numbers_job {
        job.running = false;
        job.error = error;
    }
    drop(s);
    ctx.request_repaint();
}

/// Poll the queue directory for paths dropped in by the Finder Quick Action.
fn spawn_queue_watcher(shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        if let Ok(entries) = std::fs::read_dir(queue_dir()) {
            let mut picked_up = false;
            for entry in entries.flatten() {
                let marker = entry.path();
                match marker.extension().and_then(|e| e.to_str()) {
                    Some("path") => {
                        let Ok(content) = std::fs::read_to_string(&marker) else { continue };
                        let _ = std::fs::remove_file(&marker);
                        let target = PathBuf::from(content.trim());
                        if target.exists() {
                            eprintln!("[queue] picked up {}", target.display());
                            let mut s = shared.lock().unwrap();
                            if !s.pending_files.contains(&target) {
                                s.pending_files.push(target);
                                picked_up = true;
                            }
                        } else {
                            eprintln!("[queue] ignoring missing path {}", target.display());
                        }
                    }
                    Some("text") => {
                        let Ok(text) = std::fs::read_to_string(&marker) else { continue };
                        let _ = std::fs::remove_file(&marker);
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            eprintln!("[queue] picked up selection ({} chars)", text.chars().count());
                            try_start_text_job(&shared, &ctx, text);
                        }
                    }
                    _ => continue,
                }
            }
            if picked_up {
                ctx.request_repaint();
            }
        }
        // start the next queued job here, NOT in the UI pass — rendering is
        // skipped while the window is hidden/occluded, but the queue must
        // keep flowing in the background
        let next = {
            let mut s = shared.lock().unwrap();
            let idle = s.file_job.as_ref().is_none_or(|j| !j.running);
            if idle && !s.pending_files.is_empty() {
                Some(s.pending_files.remove(0))
            } else {
                None
            }
        };
        if let Some(path) = next {
            try_start_file_job(&shared, &ctx, path);
        }
        std::thread::sleep(Duration::from_millis(800));
    });
}

/// `data.xlsx` + "Indonesian" → `data.indonesian.xlsx` (spreadsheets always
/// keep their own extension — the output is a real workbook).
pub fn spreadsheet_out_path(path: &std::path::Path, target_lang: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("xlsx");
    let lang = target_lang.to_lowercase().replace(' ', "-");
    path.with_file_name(format!("{stem}.{lang}.{ext}"))
}

/// `report.pdf` + "Indonesian" → `report.indonesian.txt` (keeps the original
/// extension for plain-text sources, `.txt` for extracted formats).
fn translated_path(path: &PathBuf, target_lang: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt")
        .to_ascii_lowercase();
    let keep_ext = !matches!(
        ext.as_str(),
        "pdf" | "docx" | "doc" | "rtf" | "rtfd" | "odt" | "webarchive"
    );
    let out_ext = if keep_ext { ext } else { "txt".into() };
    let lang = target_lang.to_lowercase().replace(' ', "-");
    path.with_file_name(format!("{stem}.{lang}.{out_ext}"))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut toggle_captions = false;
        let mut toggle_mask = false;
        let mut pick_file = false;
        let mut start_numbers = false;
        let mut start_pdf = false;
        let mut dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .first()
                .map(|f| f.path().to_path_buf())
        });
        let mut s = self.shared.lock().unwrap();
        if s.file_window_requested {
            self.show_file_window = true;
            s.file_window_requested = false;
        }

        egui::Panel::top("controls")
            .frame(egui::Frame::NONE.fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ui, |ui| {
            // row 1: capture source (the OS titlebar already shows the app name,
            // so no in-app title row — every pixel here is a live control)
            ui.horizontal(|ui| {
                let mark_size = egui::vec2(20.0, 20.0);
                let (rect, _) = ui.allocate_exact_size(mark_size, egui::Sense::hover());
                theme::gradient_rect(ui.painter(), rect, 6.0);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "文",
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
                let selected_label = s
                    .selected
                    .as_ref()
                    .map(|t| t.label.clone())
                    .unwrap_or_else(|| "Choose a source…".into());
                let targets = s.targets.clone();
                egui::ComboBox::from_id_salt("target_picker")
                    .selected_text(selected_label)
                    .width((ui.available_width() - 34.0).max(70.0))
                    .show_ui(ui, |ui| {
                        for t in &targets {
                            let checked = s.selected.as_ref() == Some(t);
                            if ui.selectable_label(checked, &t.label).clicked() {
                                s.selected = Some(t.clone());
                            }
                        }
                    });
                if ui.button("🔄").on_hover_text("Refresh window list").clicked() {
                    s.targets = capture::list_targets();
                }
            });

            // row 2: target language + the full action toolbar — wraps onto
            // extra lines when the window is narrower than everything fits
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut s.cfg.target_lang).desired_width(76.0),
                );
                if theme::chip_toggle(ui, s.cfg.target_lang == "English", "EN").clicked() {
                    s.cfg.target_lang = "English".into();
                }
                if theme::chip_toggle(ui, s.cfg.target_lang == "Indonesian", "ID").clicked() {
                    s.cfg.target_lang = "Indonesian".into();
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(2.0);

                if theme::icon_toggle(ui, s.paused, if s.paused { "▶" } else { "⏸" })
                    .on_hover_text(if s.paused { "Resume" } else { "Pause" })
                    .clicked()
                {
                    s.paused = !s.paused;
                }
                let job_running = s.file_job.as_ref().is_some_and(|j| j.running);
                let r = ui
                    .add_enabled_ui(!job_running, |ui| theme::icon_toggle(ui, false, "📄"))
                    .inner
                    .on_hover_text("File — translate a file (or drop one on this window)");
                if r.clicked() {
                    pick_file = true;
                }
                let numbers_running = s.numbers_job.as_ref().is_some_and(|j| j.running);
                let r = ui
                    .add_enabled_ui(!numbers_running, |ui| theme::icon_toggle(ui, false, "🔢"))
                    .inner
                    .on_hover_text(
                        "Live Sheet — translate the sheet open in Numbers, in place",
                    );
                if r.clicked() {
                    start_numbers = true;
                }
                let pdf_running = s.pdf_job.as_ref().is_some_and(|j| j.running);
                let r = ui
                    .add_enabled_ui(!pdf_running, |ui| theme::icon_toggle(ui, false, "📕"))
                    .inner
                    .on_hover_text("PDF — translate the PDF open in Preview, in place");
                if r.clicked() {
                    start_pdf = true;
                }
                let captions_on = self.engine.is_some();
                if theme::icon_toggle(ui, captions_on, "🎤")
                    .on_hover_text("Captions — live-translate system audio (Zoom/Meet)")
                    .clicked()
                {
                    toggle_captions = true;
                }
                let mask_on = self.mask_engine.is_some();
                if theme::icon_toggle(ui, mask_on, "🎭")
                    .on_hover_text("Mask — cover on-screen text with the translation, in place")
                    .clicked()
                {
                    toggle_mask = true;
                }
                ui.separator();
                if theme::icon_toggle(ui, self.show_settings, "⚙")
                    .on_hover_text("Settings")
                    .clicked()
                {
                    self.show_settings = !self.show_settings;
                }
            });

            if self.show_settings {
                ui.add_space(6.0);
                egui::Frame::NONE
                    .fill(theme::ELEVATED)
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        egui::Grid::new("settings").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("Model");
                            ui.text_edit_singleline(&mut s.cfg.model);
                            ui.end_row();
                            ui.label("API base");
                            ui.text_edit_singleline(&mut s.cfg.api_base);
                            ui.end_row();
                            ui.label("API key");
                            ui.add(
                                egui::TextEdit::singleline(&mut s.cfg.api_key).password(true),
                            );
                            ui.end_row();
                            ui.label("Interval (s)");
                            ui.add(egui::Slider::new(&mut s.cfg.interval_secs, 1.0..=15.0));
                            ui.end_row();
                            ui.label("Whisper model");
                            ui.text_edit_singleline(&mut s.cfg.whisper_model);
                            ui.end_row();
                        });
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("SPOKEN LANGUAGES").small().color(theme::TEXT_MUTED),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Pick as many as you like — Whisper detects which one is \
                                 spoken per utterance, so mixed-language meetings just work. \
                                 Pick exactly one only if every speaker uses that language, \
                                 for slightly better accuracy.",
                            )
                            .size(11.0)
                            .color(theme::TEXT_MUTED),
                        );
                        ui.add_space(4.0);

                        let mut selected: Vec<String> = s
                            .cfg
                            .stt_lang
                            .split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect();
                        let is_auto = selected.is_empty() || selected.iter().any(|c| c == "auto");

                        ui.horizontal_wrapped(|ui| {
                            if theme::chip_toggle(ui, is_auto, "Auto — any language").clicked() {
                                s.cfg.stt_lang = "auto".into();
                            }
                            ui.separator();
                            for (code, name) in stt::COMMON_LANGUAGES {
                                let active = !is_auto && selected.iter().any(|c| c == code);
                                if theme::chip_toggle(ui, active, name).clicked() {
                                    selected.retain(|c| c != "auto");
                                    if active {
                                        selected.retain(|c| c != *code);
                                    } else {
                                        selected.push((*code).to_string());
                                    }
                                    s.cfg.stt_lang =
                                        if selected.is_empty() { "auto".into() } else { selected.join(",") };
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("+ other code").small().color(theme::TEXT_MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_lang_input)
                                    .desired_width(60.0)
                                    .hint_text("e.g. sw"),
                            );
                            if theme::chip_toggle(ui, false, "Add").clicked()
                                && !self.custom_lang_input.trim().is_empty()
                            {
                                let code = self.custom_lang_input.trim().to_lowercase();
                                selected.retain(|c| c != "auto");
                                if !selected.iter().any(|c| *c == code) {
                                    selected.push(code);
                                }
                                s.cfg.stt_lang = selected.join(",");
                                self.custom_lang_input.clear();
                            }
                        });
                        ui.add_space(6.0);
                        if theme::chip_toggle(ui, false, "💾 Save settings").clicked() {
                            if let Err(e) = s.cfg.save() {
                                s.status = format!("⚠ Could not save config: {e:#}");
                            } else {
                                s.status = "Settings saved".into();
                            }
                        }
                        }); // ScrollArea
                    });
            }
        });

        if self.engine.is_some() || !s.captions.is_empty() {
            egui::Panel::bottom("captions_panel")
                .resizable(true)
                .default_size(180.0)
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Live captions").strong());
                        ui.label(egui::RichText::new(&s.caption_status).small().weak());
                        if !s.captions.is_empty() && ui.small_button("Clear").clicked() {
                            s.captions.clear();
                        }
                    });
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for c in &s.captions {
                                ui.label(
                                    egui::RichText::new(&c.original).size(12.0).weak(),
                                );
                                match &c.translation {
                                    Some(t) => {
                                        ui.label(egui::RichText::new(t).size(15.0));
                                    }
                                    None => {
                                        ui.label(
                                            egui::RichText::new("…").size(15.0).weak(),
                                        );
                                    }
                                }
                                ui.add_space(6.0);
                            }
                        });
                });
        }

        egui::Panel::bottom("status")
            .frame(egui::Frame::NONE.fill(theme::PANEL).inner_margin(egui::Margin::symmetric(16, 8)))
            .show(ui, |ui| {
            egui::Sides::new().show(
                ui,
                |ui| {
                    // truncates instead of overlapping the timestamp when narrow
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&s.status).small().color(theme::TEXT_MUTED),
                        )
                        .truncate(),
                    );
                },
                |ui| {
                    if let Some(t) = s.last_update {
                        ui.label(
                            egui::RichText::new(format!(
                                "updated {}s ago",
                                t.elapsed().as_secs()
                            ))
                            .small()
                            .color(theme::TEXT_MUTED),
                        );
                    }
                },
            );
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                if s.translation.is_empty() {
                    let avail = ui.available_size();
                    ui.allocate_ui_with_layout(
                        avail,
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            ui.add_space((avail.y * 0.18).max(16.0));
                            let mark = egui::vec2(40.0, 40.0);
                            let (rect, _) = ui.allocate_exact_size(mark, egui::Sense::hover());
                            theme::gradient_rect(ui.painter(), rect, 12.0);
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "文→A",
                                egui::FontId::proportional(12.0),
                                egui::Color32::WHITE,
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("Translations will appear here")
                                    .size(14.5)
                                    .strong()
                                    .color(theme::TEXT),
                            );
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new(
                                    "Pick a source above, or use File / PDF / Sheet / Mask.",
                                )
                                .size(11.5)
                                .color(theme::TEXT_MUTED),
                            );
                        },
                    );
                } else {
                    egui::Frame::NONE
                        .fill(theme::PANEL)
                        .corner_radius(egui::CornerRadius::same(14))
                        .inner_margin(egui::Margin::same(18))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(&s.translation).size(16.0).color(theme::TEXT));
                            ui.add_space(8.0);
                            ui.checkbox(&mut self.show_original, "Show original text");
                            if self.show_original {
                                ui.separator();
                                ui.label(
                                    egui::RichText::new(&s.original).size(13.0).color(theme::TEXT_MUTED),
                                );
                            }
                        });
                }
            });
        });

        if let Some(job) = &s.pdf_job {
            egui::Window::new("📕 PDF in place")
                .default_size([340.0, 110.0])
                .show(&ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(egui::RichText::new(&job.doc_name).strong());
                    if job.running {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            if job.total > 0 {
                                ui.add(
                                    egui::ProgressBar::new(job.done as f32 / job.total as f32)
                                        .text(format!("{}/{} batches", job.done, job.total)),
                                );
                            } else {
                                ui.label("reading PDF…");
                            }
                        });
                    } else if let Some(err) = &job.error {
                        ui.colored_label(ui.visuals().warn_fg_color, err);
                    } else {
                        ui.label(format!(
                            "Done — {} line(s) covered with translations, in the same file.",
                            job.masked
                        ));
                        ui.label(
                            egui::RichText::new(
                                "The original text is still underneath (annotations). \
                                 Click 📕 PDF again anytime to re-translate.",
                            )
                            .small()
                            .weak(),
                        );
                    }
                    }); // ScrollArea
                });
        }

        if let Some(job) = &s.numbers_job {
            egui::Window::new("🔢 Live Sheet")
                .default_size([340.0, 120.0])
                .show(&ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(egui::RichText::new(&job.doc_name).strong());
                    ui.label(egui::RichText::new(&job.sheet_name).weak());
                    if job.running {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            if job.total > 0 {
                                ui.add(
                                    egui::ProgressBar::new(job.done as f32 / job.total as f32)
                                        .text(format!("{}/{} cells", job.done, job.total)),
                                );
                            } else {
                                ui.label("reading sheet…");
                            }
                        });
                    } else if let Some(err) = &job.error {
                        ui.colored_label(ui.visuals().warn_fg_color, err);
                    } else if job.total == 0 {
                        ui.label("Nothing to translate on this sheet.");
                    } else {
                        ui.label(format!("Done — {} cell(s) updated in place.", job.done));
                        ui.label(
                            egui::RichText::new(
                                "Switch tabs in Numbers, then click 🔢 Live Sheet again \
                                 for the next sheet. Cmd+Z in Numbers undoes this.",
                            )
                            .small()
                            .weak(),
                        );
                    }
                    }); // ScrollArea
                });
        }

        let mut do_continue = false;
        let mut do_all = false;
        if self.show_file_window {
            let mut open = true;
            egui::Window::new("\u{1F4C4} Quick View")
                .open(&mut open)
                .default_size([440.0, 400.0])
                .show(&ctx, |ui| {
                    let cancel_flag = s.file_cancel.clone();
                    let Some(job) = &mut s.file_job else {
                        ui.label("Right-click a file in Finder \u{2192} Translate with Polyglot.");
                        return;
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(
                                job.path.file_name().unwrap_or_default().to_string_lossy(),
                            )
                            .strong(),
                        );
                        if job.total > 0 {
                            ui.label(
                                egui::RichText::new(format!("part {}/{}", job.done, job.total))
                                    .small()
                                    .weak(),
                            );
                        }
                    });
                    if job.running {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("translating\u{2026}").weak());
                            if ui.button("Cancel").clicked() {
                                cancel_flag.store(true, Ordering::Relaxed);
                            }
                        });
                    } else {
                        if let Some(err) = &job.error {
                            ui.colored_label(ui.visuals().warn_fg_color, err);
                        }
                        ui.horizontal(|ui| {
                            let more = job.done < job.total;
                            if more && ui.button("\u{25B8} Continue").clicked() {
                                do_continue = true;
                            }
                            if !job.is_selection
                                && job.saved_to.is_none()
                                && ui.button("\u{1F4BE} Translate all & save").clicked()
                            {
                                do_all = true;
                            }
                            if !job.result.is_empty() && ui.button("\u{1F4CB} Copy").clicked() {
                                ctx.copy_text(job.result.clone());
                            }
                            if let Some(p) = &job.saved_to {
                                ui.label(
                                    egui::RichText::new(format!("saved: {}", p.display()))
                                        .small()
                                        .weak(),
                                );
                            }
                        });
                    }
                    if !job.result.is_empty() {
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .stick_to_bottom(job.running)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(&job.result).size(14.0));
                            });
                    }
                });
            self.show_file_window = open;
        }

        drop(s);
        if do_continue {
            continue_quick(&self.shared, &ctx);
        }
        if do_all {
            translate_all_and_save(&self.shared, &ctx);
        }
        if start_numbers {
            start_numbers_job(&self.shared, &ctx);
        }
        if start_pdf {
            start_pdf_job(&self.shared, &ctx);
        }
        if let Some(path) = dropped.take() {
            try_start_file_job(&self.shared, &ctx, path);
        }
        if pick_file {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                try_start_file_job(&self.shared, &ctx, path);
            }
        }
        if toggle_captions {
            match self.engine.take() {
                Some(mut engine) => {
                    engine.stop();
                    self.shared.lock().unwrap().caption_status = "Captions off".into();
                }
                None => match CaptionEngine::start(self.shared.clone(), ctx.clone()) {
                    Ok(engine) => self.engine = Some(engine),
                    Err(e) => {
                        self.shared.lock().unwrap().caption_status = format!("⚠ {e:#}");
                    }
                },
            }
        }
        if toggle_mask {
            match self.mask_engine.take() {
                Some(_engine) => {
                    let mut s = self.shared.lock().unwrap();
                    s.mask_lines.clear();
                    s.mask_target_rect = None;
                    s.mask_texture = None;
                }
                None => {
                    // window-specific targets aren't reliably enumerable on
                    // this macOS version (see capture.rs), so "just pick a
                    // window" isn't a safe default — but a monitor always
                    // works, so auto-pick one rather than silently doing
                    // nothing when the user hasn't chosen a source yet.
                    let have_target = {
                        let mut s = self.shared.lock().unwrap();
                        if s.selected.is_none() {
                            s.selected = s.targets.iter().find(|t| t.is_monitor).cloned();
                        }
                        if s.selected.is_none() {
                            s.status = "⚠ No screen found to capture".into();
                        }
                        s.selected.is_some()
                    };
                    if have_target {
                        self.mask_engine =
                            Some(mask::MaskEngine::start(self.shared.clone(), ctx.clone()));
                    }
                }
            }
        }
        if self.mask_engine.is_some() {
            mask::show_mask_viewport(&ctx, &self.shared);
        }

        // keep the "updated Xs ago" label ticking
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn on_exit(&mut self) {
        let _ = self.shared.lock().unwrap().cfg.save();
    }

    /// eframe's default is near-opaque black (rgba 12,12,12,180) — applied to
    /// every viewport, which silently defeats `with_transparent(true)` on the
    /// 🎭 Mask overlay (it renders as a solid dark rectangle instead of a true
    /// see-through window). Every panel in the main window already paints its
    /// own opaque background explicitly (`theme::PANEL`/`theme::BG` frame
    /// fills), so a fully transparent GPU clear doesn't change its look —
    /// it only fixes the overlay, which has nothing else to fall back on.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

/// egui's default fonts have no CJK glyphs; pull in Arial Unicode so the
/// original Mandarin text renders instead of showing boxes.
fn install_cjk_font(ctx: &egui::Context) {
    let path = "/System/Library/Fonts/Supplemental/Arial Unicode.ttf";
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("arial_unicode".into(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("arial_unicode".into());
    }
    ctx.set_fonts(fonts);
}

fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn spawn_worker(shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut last_hash = 0u64;
        loop {
            let (cfg, target, paused) = {
                let s = shared.lock().unwrap();
                (s.cfg.clone(), s.selected.clone(), s.paused)
            };

            let Some(target) = target else {
                std::thread::sleep(Duration::from_millis(400));
                continue;
            };
            if paused || cfg.api_key.is_empty() {
                std::thread::sleep(Duration::from_millis(400));
                continue;
            }

            let interval = Duration::from_secs_f32(cfg.interval_secs.max(1.0));
            let started = Instant::now();

            let text = match capture::capture(&target).and_then(|img| {
                let lines = ocr::recognize(&img)?;
                Ok(ocr::lines_to_text(&lines, cfg.min_confidence, cfg.max_chars))
            }) {
                Ok(text) => text,
                Err(e) => {
                    shared.lock().unwrap().status = format!("⚠ {e:#}");
                    ctx.request_repaint();
                    std::thread::sleep(interval);
                    continue;
                }
            };

            if text.is_empty() {
                shared.lock().unwrap().status = "No text detected on screen".into();
                ctx.request_repaint();
                std::thread::sleep(interval);
                continue;
            }

            let hash = text_hash(&text);
            if hash == last_hash {
                std::thread::sleep(interval);
                continue;
            }

            {
                shared.lock().unwrap().status = "Translating…".into();
                ctx.request_repaint();
            }

            match translate::translate(&cfg, &text) {
                Ok(translation) => {
                    let mut s = shared.lock().unwrap();
                    s.original = text;
                    s.translation = translation;
                    s.status = format!("✓ Translated in {:.1}s", started.elapsed().as_secs_f32());
                    s.last_update = Some(Instant::now());
                    last_hash = hash;
                }
                Err(e) => {
                    // leave last_hash unchanged so the next cycle retries
                    shared.lock().unwrap().status = format!("⚠ {e:#}");
                }
            }
            ctx.request_repaint();
            std::thread::sleep(interval);
        }
    });
}
