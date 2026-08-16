use crate::app::Shared;
use crate::audio::{self, AudioCapture};
use crate::stt::Stt;
use crate::translate;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Caption {
    pub id: u64,
    pub original: String,
    pub translation: Option<String>,
}

/// Max captions kept in the UI list.
const MAX_CAPTIONS: usize = 60;
/// Force a segment out after this much buffered audio.
const MAX_SEGMENT_SECS: f32 = 8.0;
/// Minimum speech before a silence gap finalizes a segment.
const MIN_SEGMENT_SECS: f32 = 1.2;
/// Trailing silence that finalizes a segment.
const SILENCE_GAP_SECS: f32 = 0.7;
/// RMS below this counts as silence (system audio is usually clean).
const SILENCE_RMS: f32 = 0.004;

pub struct CaptionEngine {
    stop: Arc<AtomicBool>,
    capture: AudioCapture,
}

impl CaptionEngine {
    pub fn start(shared: Arc<Mutex<Shared>>, ctx: egui::Context) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();
        let (status_tx, status_rx) = mpsc::channel::<String>();
        let capture = AudioCapture::start(audio_tx, status_tx)?;

        // forward helper log lines (incl. permission errors) to the UI
        {
            let shared = shared.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                for line in status_rx {
                    shared.lock().unwrap().caption_status = line;
                    ctx.request_repaint();
                }
            });
        }

        // translation queue: one worker, updates captions in place by id
        let (tr_tx, tr_rx) = mpsc::channel::<(u64, String)>();
        {
            let shared = shared.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                for (id, text) in tr_rx {
                    let cfg = shared.lock().unwrap().cfg.clone();
                    let result = translate::translate_caption(&cfg, &text);
                    let mut s = shared.lock().unwrap();
                    if let Some(c) = s.captions.iter_mut().find(|c| c.id == id) {
                        c.translation = Some(match result {
                            Ok(t) => t,
                            Err(e) => format!("⚠ {e:#}"),
                        });
                    }
                    ctx.request_repaint();
                }
            });
        }

        // STT worker: segment on silence, transcribe, hand off to translation
        {
            let stop = stop.clone();
            let shared = shared.clone();
            std::thread::spawn(move || {
                stt_loop(&stop, &shared, &ctx, &audio_rx, &tr_tx);
            });
        }

        Ok(Self { stop, capture })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.capture.stop(); // closes the audio channel, ending the STT loop
    }
}

impl Drop for CaptionEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stt_loop(
    stop: &AtomicBool,
    shared: &Arc<Mutex<Shared>>,
    ctx: &egui::Context,
    audio_rx: &mpsc::Receiver<Vec<f32>>,
    tr_tx: &mpsc::Sender<(u64, String)>,
) {
    let (model_path, language) = {
        let s = shared.lock().unwrap();
        (s.cfg.whisper_model.clone(), s.cfg.stt_lang.clone())
    };

    set_status(shared, ctx, "Loading Whisper model…");
    let mut stt = match Stt::load(&model_path, &language) {
        Ok(stt) => stt,
        Err(e) => {
            set_status(shared, ctx, &format!("⚠ {e:#}"));
            return;
        }
    };
    set_status(shared, ctx, "🎤 Listening…");

    let rate = audio::SAMPLE_RATE as f32;
    let mut buffer: Vec<f32> = Vec::new();
    let mut silence_run = 0usize; // trailing silent samples
    let mut next_id = 1u64;

    while !stop.load(Ordering::Relaxed) {
        let chunk = match audio_rx.recv_timeout(Duration::from_millis(300)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len().max(1) as f32).sqrt();
        if rms < SILENCE_RMS {
            silence_run += chunk.len();
            // don't grow the buffer with leading silence
            if buffer.is_empty() {
                continue;
            }
        } else {
            silence_run = 0;
        }
        buffer.extend_from_slice(&chunk);

        let buffered_secs = buffer.len() as f32 / rate;
        let gap_secs = silence_run as f32 / rate;
        let should_flush = buffered_secs >= MAX_SEGMENT_SECS
            || (buffered_secs >= MIN_SEGMENT_SECS + gap_secs && gap_secs >= SILENCE_GAP_SECS);
        if !should_flush {
            continue;
        }

        let segment: Vec<f32> = std::mem::take(&mut buffer);
        silence_run = 0;

        // whisper wants at least ~1s of audio to behave
        if segment.len() < rate as usize {
            continue;
        }

        match stt.transcribe(&segment) {
            Ok(text) if !text.is_empty() => {
                let id = next_id;
                next_id += 1;
                {
                    let mut s = shared.lock().unwrap();
                    s.captions.push(Caption {
                        id,
                        original: text.clone(),
                        translation: None,
                    });
                    let overflow = s.captions.len().saturating_sub(MAX_CAPTIONS);
                    if overflow > 0 {
                        s.captions.drain(..overflow);
                    }
                    s.caption_status = "🎤 Listening…".into();
                }
                ctx.request_repaint();
                let _ = tr_tx.send((id, text));
            }
            Ok(_) => {} // empty transcript (noise/music) — skip silently
            Err(e) => set_status(shared, ctx, &format!("⚠ {e:#}")),
        }
    }
}

fn set_status(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, msg: &str) {
    shared.lock().unwrap().caption_status = msg.to_string();
    ctx.request_repaint();
}
