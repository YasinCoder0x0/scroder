//! Platform independent types shared by the GUI, the CLI and every capture
//! backend.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn is_valid(&self) -> bool {
        self.width > 1 && self.height > 1
    }

    /// Clamp the region to the capture surface and make it encoder friendly
    /// (even width/height).
    pub fn normalized(&self, max_width: u32, max_height: u32) -> Option<Region> {
        if !self.is_valid() || max_width < 2 || max_height < 2 {
            return None;
        }
        let x = self.x.min(max_width.saturating_sub(2));
        let y = self.y.min(max_height.saturating_sub(2));
        let width = self.width.min(max_width.saturating_sub(x)) & !1;
        let height = self.height.min(max_height.saturating_sub(y)) & !1;
        if width < 2 || height < 2 {
            return None;
        }
        Some(Region {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "Top left",
            Corner::TopRight => "Top right",
            Corner::BottomLeft => "Bottom left",
            Corner::BottomRight => "Bottom right",
        }
    }

    pub fn from_index(index: u8) -> Corner {
        match index {
            0 => Corner::TopLeft,
            1 => Corner::TopRight,
            2 => Corner::BottomLeft,
            _ => Corner::BottomRight,
        }
    }

    pub fn index(self) -> u8 {
        match self {
            Corner::TopLeft => 0,
            Corner::TopRight => 1,
            Corner::BottomLeft => 2,
            Corner::BottomRight => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WebcamOptions {
    pub device_index: usize,
    pub size_pct: u8,
    pub corner: Corner,
    pub mirror: bool,
}

impl Default for WebcamOptions {
    fn default() -> Self {
        Self {
            device_index: 0,
            size_pct: 22,
            corner: Corner::BottomRight,
            mirror: true,
        }
    }
}

/// A physical display. Coordinates and sizes are in physical pixels.
#[derive(Clone, Debug)]
pub struct DisplaySource {
    /// Backend specific identifier (HMONITOR, CGDirectDisplayID, RandR index).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub id: u64,
    /// Zero based position in the enumeration order.
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical point.
    pub scale: f32,
    pub refresh_hz: u32,
    pub primary: bool,
}

impl DisplaySource {
    pub fn label(&self) -> String {
        let primary = if self.primary { " · Primary" } else { "" };
        if self.refresh_hz > 0 {
            format!(
                "{}  ({}×{} @ {}Hz){primary}",
                self.name, self.width, self.height, self.refresh_hz
            )
        } else {
            format!("{}  ({}×{}){primary}", self.name, self.width, self.height)
        }
    }

    pub fn logical_position(&self) -> (f32, f32) {
        let scale = self.scale.max(0.1);
        (self.x as f32 / scale, self.y as f32 / scale)
    }

    pub fn logical_size(&self) -> (f32, f32) {
        let scale = self.scale.max(0.1);
        (self.width as f32 / scale, self.height as f32 / scale)
    }
}

/// A top level application window.
#[derive(Clone, Debug)]
pub struct WindowSource {
    /// Backend specific identifier (HWND, X11 window id, CGWindowID).
    pub id: u64,
    pub title: String,
    pub app: String,
    pub width: u32,
    pub height: u32,
}

impl WindowSource {
    pub fn label(&self) -> String {
        let title = truncate(&self.title, 40);
        if self.app.is_empty() || self.app.eq_ignore_ascii_case(&self.title) {
            format!("{title}  ({}×{})", self.width, self.height)
        } else {
            format!("{} · {title}  ({}×{})", self.app, self.width, self.height)
        }
    }
}

#[derive(Clone, Debug)]
pub enum CaptureTarget {
    Display(DisplaySource),
    Window(WindowSource),
    /// The user picks the screen or window in the desktop's own
    /// screen-sharing dialog (Wayland).
    Portal {
        window: bool,
    },
}

impl CaptureTarget {
    pub fn width(&self) -> u32 {
        match self {
            CaptureTarget::Display(d) => d.width,
            CaptureTarget::Window(w) => w.width,
            CaptureTarget::Portal { .. } => 0,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            CaptureTarget::Display(d) => d.height,
            CaptureTarget::Window(w) => w.height,
            CaptureTarget::Portal { .. } => 0,
        }
    }

    pub fn label(&self) -> String {
        match self {
            CaptureTarget::Display(d) => d.label(),
            CaptureTarget::Window(w) => w.label(),
            CaptureTarget::Portal { window: false } => "Screen chosen in system dialog".into(),
            CaptureTarget::Portal { window: true } => "Window chosen in system dialog".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecordingOptions {
    pub target: CaptureTarget,
    pub output: PathBuf,
    pub fps: u32,
    pub bitrate: u32,
    pub codec: Codec,
    pub system_audio: bool,
    pub microphone: bool,
    pub capture_cursor: bool,
    pub region: Option<Region>,
    pub webcam: Option<WebcamOptions>,
}

impl RecordingOptions {
    pub fn with_defaults(target: CaptureTarget) -> Self {
        let dir = crate::util::recordings_dir();
        Self {
            target,
            output: crate::util::timestamped_path(&dir, "mp4"),
            fps: 60,
            bitrate: 12_000_000,
            codec: Codec::H264,
            system_audio: true,
            microphone: false,
            capture_cursor: true,
            region: None,
            webcam: None,
        }
    }
}

/// A single captured frame used for the region picker preview.
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA pixels, top row first.
    pub rgba: Vec<u8>,
}

/// A dependency the current platform needs before it can record.
#[derive(Clone, Debug)]
pub struct MissingDependency {
    pub title: String,
    pub detail: String,
    /// A shell command that installs the dependency, when one is known.
    pub install_command: Option<String>,
}

/// What the active backend can do on this machine.
#[derive(Clone, Debug)]
pub struct Capabilities {
    pub platform: &'static str,
    /// Short human readable description of the video encoder.
    pub encoder_label: String,
    pub window_capture: bool,
    pub region_capture: bool,
    /// The source is chosen in a system dialog instead of in ScreenRec.
    pub portal_source: bool,
    pub system_audio: bool,
    /// Explains why system audio is unavailable or what it needs.
    pub system_audio_note: Option<String>,
    pub hevc: bool,
    pub global_hotkeys: bool,
    pub hotkey_note: Option<String>,
    pub missing: Vec<MissingDependency>,
}

/// Tracks recorded time, excluding the time spent paused.
pub struct PauseClock {
    started: Instant,
    state: Mutex<(Option<Instant>, Duration)>,
}

impl PauseClock {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            state: Mutex::new((None, Duration::ZERO)),
        }
    }

    pub fn pause(&self) {
        let mut state = self.state.lock();
        if state.0.is_none() {
            state.0 = Some(Instant::now());
        }
    }

    pub fn resume(&self) {
        let mut state = self.state.lock();
        if let Some(mark) = state.0.take() {
            state.1 += mark.elapsed();
        }
    }

    /// Recorded time: wall time since start minus every pause.
    pub fn elapsed(&self) -> Duration {
        let state = self.state.lock();
        let paused_now = state.0.map(|mark| mark.elapsed()).unwrap_or_default();
        self.started
            .elapsed()
            .saturating_sub(state.1)
            .saturating_sub(paused_now)
    }
}

impl Default for PauseClock {
    fn default() -> Self {
        Self::new()
    }
}

pub fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let short: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{}…", short.trim_end())
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_is_clamped_and_even() {
        let region = Region {
            x: 100,
            y: 50,
            width: 5000,
            height: 333,
        };
        let r = region.normalized(1920, 1080).unwrap();
        assert_eq!((r.x, r.y), (100, 50));
        assert_eq!(r.width, 1820);
        assert_eq!(r.height, 332);
        assert!(Region::default().normalized(1920, 1080).is_none());
    }

    #[test]
    fn pause_clock_excludes_paused_time() {
        let clock = PauseClock::new();
        clock.pause();
        std::thread::sleep(Duration::from_millis(120));
        let paused = clock.elapsed();
        assert!(paused < Duration::from_millis(60), "{paused:?}");
        clock.resume();
        assert!(clock.elapsed() < Duration::from_millis(80));
    }

    #[test]
    fn corner_index_round_trips() {
        for corner in Corner::ALL {
            assert_eq!(Corner::from_index(corner.index()), corner);
        }
    }
}
