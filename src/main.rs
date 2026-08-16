mod app;
mod audio;
mod capture;
mod captions;
mod config;
mod doc;
mod mask;
mod numbers;
mod ocr;
mod pdfmask;
mod sheet;
mod stt;
mod theme;
mod translate;

use eframe::egui;

fn main() -> eframe::Result {
    // Headless debug mode: `cargo run -- --test-pipeline image.png`
    // runs OCR + translation on an image file and prints the result.
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, path] = args.as_slice() {
        if flag == "--test-pipeline" {
            test_pipeline(path);
            return Ok(());
        }
        // `cargo run -- --test-audio speech.wav` (16 kHz mono) runs the
        // Whisper + translation half of the captions pipeline headless.
        if flag == "--test-audio" {
            test_audio(path);
            return Ok(());
        }
        // `cargo run -- --test-file document.pdf` extracts + translates a file.
        if flag == "--test-file" {
            test_file(path);
            return Ok(());
        }
        // `cargo run -- --test-sheet data.xlsx` translates a spreadsheet.
        if flag == "--test-sheet" {
            test_sheet(path);
            return Ok(());
        }
    }
    // `cargo run -- --test-numbers` translates the active Numbers sheet in place.
    if args.len() >= 2 && args[1] == "--test-numbers" {
        test_numbers();
        return Ok(());
    }
    // `cargo run -- --test-capture` checks whether Screen Recording permission is granted.
    if args.len() >= 2 && args[1] == "--test-capture" {
        test_capture();
        return Ok(());
    }
    // `cargo run -- --test-pdfmask file.pdf` translates a PDF in place (same file).
    if let [_, flag, path] = args.as_slice() {
        if flag == "--test-pdfmask" {
            let cfg = config::Config::load().expect("loading config");
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let masked = pdfmask::translate_in_place(
                &cfg,
                std::path::Path::new(path),
                &cancel,
                &mut |done, total| eprintln!("batch {done}/{total}"),
            )
            .expect("translating PDF in place");
            println!("masked {masked} line(s) in {path}");
            return Ok(());
        }
    }
    // `cargo run -- --test-mask <window-substring>` finds the window, OCRs +
    // masks one cycle, and prints the resulting line positions/translations.
    if let [_, flag, needle] = args.as_slice() {
        if flag == "--test-mask" {
            test_mask(needle);
            return Ok(());
        }
    }
    if args.len() >= 2 && args[1] == "--test-mask-raw" {
        test_mask_raw();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Anveesa Polyglot")
            .with_inner_size([360.0, 420.0])
            .with_min_inner_size([300.0, 220.0])
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        "Anveesa Polyglot",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

fn test_pipeline(path: &str) {
    let cfg = config::Config::load().expect("loading config");
    let img = image::open(path).expect("opening image").to_rgba8();
    let lines = ocr::recognize(&img).expect("running OCR");
    let text = ocr::lines_to_text(&lines, cfg.min_confidence, cfg.max_chars);
    println!("--- OCR text ---\n{text}\n--- translation ({}) ---", cfg.target_lang);
    match translate::translate(&cfg, &text) {
        Ok(t) => println!("{t}"),
        Err(e) => eprintln!("translation failed: {e:#}"),
    }
}

fn test_audio(path: &str) {
    let cfg = config::Config::load().expect("loading config");
    let mut reader = hound::WavReader::open(path).expect("opening wav");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "wav must be 16 kHz mono");
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
    };
    println!("loaded {:.1}s of audio; loading Whisper…", samples.len() as f32 / 16_000.0);
    let mut stt = stt::Stt::load(&cfg.whisper_model, &cfg.stt_lang).expect("loading model");
    let start = std::time::Instant::now();
    let text = stt.transcribe(&samples).expect("transcribing");
    println!("--- transcript ({:.1}s) ---\n{text}", start.elapsed().as_secs_f32());
    match translate::translate_caption(&cfg, &text) {
        Ok(t) => println!("--- translation ({}) ---\n{t}", cfg.target_lang),
        Err(e) => eprintln!("translation failed: {e:#}"),
    }
}

fn test_mask_raw() {
    for w in xcap::Window::all().expect("listing windows") {
        println!(
            "app={:?} title={:?} minimized={:?} focused={:?}",
            w.app_name(),
            w.title(),
            w.is_minimized(),
            w.is_focused()
        );
    }
}

fn test_mask(needle: &str) {
    let cfg = config::Config::load().expect("loading config");
    let targets = capture::list_targets();
    println!("{} target(s) total", targets.len());
    let target = targets
        .iter()
        .find(|t| t.label.to_lowercase().contains(&needle.to_lowercase()))
        .unwrap_or_else(|| panic!("no target label contains {needle:?}. Available: {:#?}", targets.iter().map(|t| &t.label).collect::<Vec<_>>()));
    println!("target: {}", target.label);

    let img = capture::capture(target).expect("capturing target");
    println!("captured {}x{}", img.width(), img.height());

    let lines = ocr::recognize(&img).expect("running OCR");
    let kept: Vec<_> = lines
        .into_iter()
        .filter(|l| l.confidence >= cfg.min_confidence && !l.text.trim().is_empty())
        .collect();
    println!("{} OCR line(s) above confidence threshold", kept.len());
    for l in kept.iter().take(5) {
        println!("  box={:?} text={:?}", l.bbox, l.text.chars().take(30).collect::<String>());
    }

    let texts: Vec<String> = kept.iter().map(|l| l.text.clone()).collect();
    let translated = translate::translate_batch(&cfg, &cfg.fast_model, &texts).expect("translating batch");
    println!("--- all translated lines ---");
    for (l, t) in kept.iter().zip(&translated) {
        println!("  {:?} -> {:?}", l.text.chars().take(30).collect::<String>(), t);
    }

    if !target.is_monitor {
        let window = xcap::Window::all()
            .expect("listing windows")
            .into_iter()
            .find(|w| w.title().map(|t| t.to_lowercase().contains(&needle.to_lowercase())).unwrap_or(false))
            .expect("re-finding window for position");
        println!(
            "window rect (points): x={} y={} w={} h={}",
            window.x().unwrap(), window.y().unwrap(), window.width().unwrap(), window.height().unwrap()
        );
    }
}

fn test_capture() {
    let targets = capture::list_targets();
    println!("{} capture target(s) visible", targets.len());
    let Some(monitor) = targets.iter().find(|t| t.is_monitor) else {
        println!("no monitor target found");
        return;
    };
    println!("capturing: {}", monitor.label);
    match capture::capture(monitor) {
        Ok(img) => {
            let (w, h) = (img.width(), img.height());
            let sample: Vec<u32> = img
                .pixels()
                .step_by(997)
                .take(2000)
                .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
                .collect();
            let unique: std::collections::HashSet<_> = sample.iter().collect();
            println!("captured {w}x{h}, {} unique colors in {} sampled pixels", unique.len(), sample.len());
            if unique.len() <= 2 {
                println!("⚠ looks BLANK/BLACK — Screen Recording permission is likely NOT granted");
            } else {
                println!("✓ real content captured — Screen Recording permission IS granted");
            }
        }
        Err(e) => println!("⚠ capture failed: {e:#}"),
    }
}

fn test_numbers() {
    let cfg = config::Config::load().expect("loading config");
    let info = numbers::active_sheet_info().expect("reading active Numbers sheet info");
    println!("{} / {} ({} x {})", info.doc_name, info.sheet_name, info.rows, info.cols);

    let cells = numbers::read_translatable_cells(&info).expect("reading cells");
    let cells: Vec<_> = cells.into_iter().filter(|c| !c.text.contains('\r')).collect();
    println!("{} translatable cell(s)", cells.len());

    let mut unique: Vec<String> = Vec::new();
    for c in &cells {
        if !unique.contains(&c.text) {
            unique.push(c.text.clone());
        }
    }
    println!("{} unique text(s)", unique.len());

    let mut translated = std::collections::HashMap::new();
    for chunk in unique.chunks(40) {
        let out = translate::translate_batch(&cfg, &cfg.fast_model, chunk).expect("translating");
        for (o, t) in chunk.iter().zip(out) {
            println!("  {o} -> {t}");
            translated.insert(o.clone(), t);
        }
    }

    let writes: Vec<(String, String, String)> = cells
        .into_iter()
        .filter_map(|c| translated.get(&c.text).map(|t| (c.cell_ref, c.text, t.clone())))
        .collect();
    for chunk in writes.chunks(150) {
        numbers::write_stacked_cells(chunk).expect("writing back to Numbers");
    }
    println!("wrote {} cell(s) back into the live document", writes.len());
}

fn test_sheet(path: &str) {
    let cfg = config::Config::load().expect("loading config");
    let p = std::path::Path::new(path);
    let out = app::spreadsheet_out_path(p, &cfg.target_lang);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let outcome = sheet::translate_spreadsheet(&cfg, p, &out, &cancel, &mut |done, total| {
        eprintln!("batch {done}/{total}");
    })
    .expect("translating spreadsheet");
    println!("{} unique cell value(s) → {}", outcome.cells, outcome.out_path.display());
    for (o, t) in &outcome.preview {
        println!("• {o} → {t}");
    }
}

fn test_file(path: &str) {
    let cfg = config::Config::load().expect("loading config");
    let text = doc::extract_text(std::path::Path::new(path)).expect("extracting text");
    let chunks = doc::chunk_text(&text, 3000);
    println!("extracted {} chars → {} chunk(s)", text.chars().count(), chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        match translate::translate_document(&cfg, chunk) {
            Ok(t) => println!("--- part {}/{} ({}) ---\n{t}", i + 1, chunks.len(), cfg.target_lang),
            Err(e) => {
                eprintln!("part {} failed: {e:#}", i + 1);
                return;
            }
        }
    }
}
