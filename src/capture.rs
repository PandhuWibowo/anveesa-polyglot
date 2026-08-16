use anyhow::{anyhow, Result};
use xcap::image::RgbaImage;
use xcap::{Monitor, Window};

#[derive(Debug, Clone, PartialEq)]
pub struct CaptureTarget {
    pub id: u32,
    pub label: String,
    /// true = whole monitor, false = single window
    pub is_monitor: bool,
}

pub fn list_targets() -> Vec<CaptureTarget> {
    let mut targets = Vec::new();

    if let Ok(monitors) = Monitor::all() {
        for m in monitors {
            let (Ok(id), Ok(name)) = (m.id(), m.name()) else {
                continue;
            };
            targets.push(CaptureTarget {
                id,
                label: format!("🖥 Screen: {name}"),
                is_monitor: true,
            });
        }
    }

    if let Ok(windows) = Window::all() {
        for w in windows {
            let (Ok(id), Ok(title), Ok(app)) = (w.id(), w.title(), w.app_name()) else {
                continue;
            };
            if w.is_minimized().unwrap_or(true) || title.trim().is_empty() {
                continue;
            }
            let mut label = format!("{app} — {title}");
            if label.chars().count() > 60 {
                label = label.chars().take(60).collect::<String>() + "…";
            }
            targets.push(CaptureTarget {
                id,
                label,
                is_monitor: false,
            });
        }
    }

    targets
}

pub fn capture(target: &CaptureTarget) -> Result<RgbaImage> {
    if target.is_monitor {
        let monitor = Monitor::all()?
            .into_iter()
            .find(|m| m.id().map(|id| id == target.id).unwrap_or(false))
            .ok_or_else(|| anyhow!("monitor no longer available"))?;
        Ok(monitor.capture_image()?)
    } else {
        let window = Window::all()?
            .into_iter()
            .find(|w| w.id().map(|id| id == target.id).unwrap_or(false))
            .ok_or_else(|| anyhow!("window no longer available (closed?)"))?;
        Ok(window.capture_image()?)
    }
}
