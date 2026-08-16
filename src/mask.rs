use crate::app::Shared;
use crate::capture::{self, CaptureTarget};
use crate::{ocr, translate};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One translated line, positioned as a fraction of the target window's
/// on-screen rect (top-left origin) — ready to draw without knowing pixels.
#[derive(Clone)]
pub struct MaskLine {
    /// (left, top, width, height) as fractions [0.0, 1.0] of the window.
    pub rect_frac: (f32, f32, f32, f32),
    pub translated: String,
}

pub struct MaskEngine {
    stop: Arc<AtomicBool>,
}

impl MaskEngine {
    pub fn start(shared: Arc<Mutex<Shared>>, ctx: egui::Context) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = stop.clone();
            std::thread::spawn(move || mask_loop(&stop, &shared, &ctx));
        }
        Self { stop }
    }
}

impl Drop for MaskEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Whole-screen capture inevitably sees macOS menu-bar icons (Wi-Fi, battery,
/// clock glyphs — misread by OCR as 1-2 stray letters) and this very app's
/// own window ("Anveesa Polyglot" is on screen too). Neither is worth
/// translating or boxing over.
fn looks_like_ui_noise(text: &str) -> bool {
    let t = text.trim();
    if t.eq_ignore_ascii_case("Anveesa Polyglot") || t.eq_ignore_ascii_case("Anveesa") {
        return true;
    }
    let char_count = t.chars().count();
    char_count <= 2 && t.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Vision's box is bottom-left-origin, normalized; window overlays want
/// top-left-origin fractions to match typical screen coordinates.
fn flip_to_top_left(bbox: [f32; 4]) -> (f32, f32, f32, f32) {
    let [x, y, w, h] = bbox;
    (x, 1.0 - y - h, w, h)
}

fn mask_loop(stop: &AtomicBool, shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    let mut last_text_hash = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let (cfg, target, interval) = {
            let s = shared.lock().unwrap();
            (s.cfg.clone(), s.selected.clone(), s.cfg.interval_secs)
        };
        let Some(target) = target else {
            std::thread::sleep(Duration::from_millis(400));
            continue;
        };

        let result = capture::capture(&target).and_then(|img| {
            let lines = ocr::recognize(&img)?;
            Ok((img, lines))
        });
        let (img, lines) = match result {
            Ok(v) => v,
            Err(e) => {
                shared.lock().unwrap().status = format!("⚠ {e:#}");
                ctx.request_repaint();
                std::thread::sleep(Duration::from_secs_f32(interval.max(1.0)));
                continue;
            }
        };

        // Paint the actual captured frame as the overlay's background instead
        // of relying on true OS-level window transparency: multi-viewport
        // transparency compositing turned out to be unreliable in practice
        // (worked in some window arrangements, rendered solid black in
        // others) — a snapshot texture behind the translated boxes is
        // correct every time, at the cost of the background only updating
        // once per capture cycle instead of being truly live.
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [img.width() as usize, img.height() as usize],
            img.as_raw(),
        );
        let texture = ctx.load_texture("mask_bg", color_image, egui::TextureOptions::LINEAR);
        {
            let mut s = shared.lock().unwrap();
            s.mask_texture = Some(texture);
            s.mask_target_rect = window_rect(&target);
        }

        let kept: Vec<_> = lines
            .into_iter()
            .filter(|l| l.confidence >= cfg.min_confidence && !l.text.trim().is_empty())
            .filter(|l| !looks_like_ui_noise(&l.text))
            .collect();

        // skip re-translating if the visible text hasn't changed since last cycle
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for l in &kept {
            l.text.hash(&mut hasher);
        }
        let hash = hasher.finish();
        if hash == last_text_hash {
            // text is the same, but the background snapshot just refreshed —
            // repaint so the new pixels (e.g. scrolled position) show up
            ctx.request_repaint();
            std::thread::sleep(Duration::from_secs_f32(interval.max(1.0)));
            continue;
        }

        if kept.is_empty() {
            let mut s = shared.lock().unwrap();
            s.mask_lines.clear();
            drop(s);
            ctx.request_repaint();
            last_text_hash = hash;
            std::thread::sleep(Duration::from_secs_f32(interval.max(1.0)));
            continue;
        }

        {
            let mut s = shared.lock().unwrap();
            s.status = "Masking…".into();
        }
        ctx.request_repaint();

        let texts: Vec<String> = kept.iter().map(|l| l.text.clone()).collect();
        match translate::translate_batch(&cfg, &cfg.fast_model, &texts) {
            Ok(translated) => {
                let mask_lines: Vec<MaskLine> = kept
                    .iter()
                    .zip(translated)
                    .map(|(l, t)| MaskLine {
                        rect_frac: flip_to_top_left(l.bbox),
                        translated: t,
                    })
                    .collect();
                let mut s = shared.lock().unwrap();
                s.mask_lines = mask_lines;
                s.status = format!("✓ Masking {} line(s)", texts.len());
                last_text_hash = hash;
            }
            Err(e) => {
                shared.lock().unwrap().status = format!("⚠ {e:#}");
            }
        }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_secs_f32(interval.max(1.0)));
    }
}

/// The target's current on-screen rect in points: (x, y, width, height).
/// For a monitor this is its full bounds; for a window, its live frame
/// (re-read every cycle so moving/resizing the window is picked up).
fn window_rect(target: &CaptureTarget) -> Option<(f32, f32, f32, f32)> {
    if target.is_monitor {
        let monitor = xcap::Monitor::all()
            .ok()?
            .into_iter()
            .find(|m| m.id().map(|id| id == target.id).unwrap_or(false))?;
        Some((
            monitor.x().ok()? as f32,
            monitor.y().ok()? as f32,
            monitor.width().ok()? as f32,
            monitor.height().ok()? as f32,
        ))
    } else {
        let window = xcap::Window::all()
            .ok()?
            .into_iter()
            .find(|w| w.id().map(|id| id == target.id).unwrap_or(false))?;
        Some((
            window.x().ok()? as f32,
            window.y().ok()? as f32,
            window.width().ok()? as f32,
            window.height().ok()? as f32,
        ))
    }
}

/// Paints the mask overlay as a borderless, click-through viewport
/// positioned exactly over the target window/screen. The background is the
/// actual captured frame (not a see-through window — see the comment in
/// `mask_loop` for why), so the result is correct regardless of window
/// manager/compositor quirks; it just means the backdrop is a snapshot that
/// refreshes once per capture cycle rather than a live view.
pub fn show_mask_viewport(ctx: &egui::Context, shared: &Arc<Mutex<Shared>>) {
    let (rect, lines, texture) = {
        let s = shared.lock().unwrap();
        (s.mask_target_rect, s.mask_lines.clone(), s.mask_texture.clone())
    };
    let Some((x, y, w, h)) = rect else { return };
    if w <= 0.0 || h <= 0.0 {
        return;
    }

    let id = egui::ViewportId::from_hash_of("anveesa_mask_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title("Anveesa Mask")
        .with_position([x, y])
        .with_inner_size([w, h])
        .with_decorations(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true);

    ctx.show_viewport_immediate(id, builder, |ctx, _class| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let full = egui::Rect::from_min_size(ui.min_rect().min, egui::vec2(w, h));
                if let Some(tex) = &texture {
                    ui.painter().image(
                        tex.id(),
                        full,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                let painter = ui.painter();
                for line in &lines {
                    let (lx, ly, lw, lh) = line.rect_frac;
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(lx * w, ly * h),
                        egui::vec2((lw * w).max(4.0), (lh * h).max(4.0)),
                    );
                    // opaque light box masks the original text underneath
                    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(250, 250, 248));
                    let font_size = (rect.height() * 0.72).clamp(8.0, 48.0);
                    painter.text(
                        rect.left_center() + egui::vec2(2.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &line.translated,
                        egui::FontId::proportional(font_size),
                        egui::Color32::from_rgb(20, 20, 20),
                    );
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::looks_like_ui_noise;

    #[test]
    fn filters_own_app_name() {
        assert!(looks_like_ui_noise("Anveesa Polyglot"));
        assert!(looks_like_ui_noise("  Anveesa Polyglot  "));
    }

    #[test]
    fn filters_short_ascii_glyph_misreads() {
        assert!(looks_like_ui_noise("K"));
        assert!(looks_like_ui_noise("00"));
    }

    #[test]
    fn keeps_real_short_content() {
        // single/double CJK characters are often meaningful (e.g. "十" = ten,
        // "京" = capital) — must not be swept up by the short-fragment filter
        assert!(!looks_like_ui_noise("十"));
        assert!(!looks_like_ui_noise("北京"));
        assert!(!looks_like_ui_noise("Beijing"));
    }
}
