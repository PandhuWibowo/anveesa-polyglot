use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;

// Compiled by build.rs from audio/main.m
const AUDIO_HELPER: &str = concat!(env!("OUT_DIR"), "/anveesa-audio");

pub const SAMPLE_RATE: usize = 16_000;

/// Spawns the ScreenCaptureKit system-audio helper and forwards its PCM
/// stream (f32le mono 16 kHz) as sample chunks over `tx`.
///
/// The helper exits on its own when this process dies (it watches stdin),
/// but `stop()` kills it explicitly when captions are toggled off.
pub struct AudioCapture {
    child: Child,
}

impl AudioCapture {
    pub fn start(tx: Sender<Vec<f32>>, status: Sender<String>) -> Result<Self> {
        let mut child = Command::new(AUDIO_HELPER)
            .stdin(Stdio::piped()) // held open; closing it stops the helper
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawning the system-audio helper")?;

        let mut stdout = child.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 3200 * 4]; // 200 ms of f32 samples
            let mut pending: Vec<u8> = Vec::new();
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        let whole = pending.len() / 4 * 4;
                        let samples: Vec<f32> = pending[..whole]
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect();
                        pending.drain(..whole);
                        if tx.send(samples).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let stderr = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = status.send(line);
            }
        });

        Ok(Self { child })
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
