use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Margin, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2,
};

use crate::backend::{self, ActiveRecording};
use crate::editor::{self, Editor, EditorEvent};
use crate::history::{self, HistoryEntry};
use crate::hotkey::{HotkeyAction, HotkeyListener};
use crate::icons::{self, Icon};
use crate::media::{
    truncate, Capabilities, CaptureTarget, Codec, Corner, DisplaySource, PreviewFrame,
    RecordingOptions, Region, WebcamOptions, WindowSource,
};
use crate::shell;
use crate::theme::{self, pal};
use crate::util;

const HISTORY_PREVIEW: usize = 5;
const RECORDING_BAR_TITLE: &str = "Scroder recording controls";
/// Outer page margin and the gap between columns.
pub(crate) const GUTTER: f32 = 16.0;
/// Height shared by buttons, inputs and dropdowns that sit side by side.
pub(crate) const CONTROL_H: f32 = 32.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetMode {
    Monitor,
    Window,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quality {
    Low,
    Balanced,
    High,
    Ultra,
}

impl Quality {
    const ALL: [Quality; 4] = [
        Quality::Low,
        Quality::Balanced,
        Quality::High,
        Quality::Ultra,
    ];

    fn label(self) -> &'static str {
        match self {
            Quality::Low => "Low · smallest files",
            Quality::Balanced => "Balanced",
            Quality::High => "High",
            Quality::Ultra => "Ultra · largest files",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Quality::Low => "Low",
            Quality::Balanced => "Balanced",
            Quality::High => "High",
            Quality::Ultra => "Ultra",
        }
    }

    fn bpp(self) -> f32 {
        match self {
            Quality::Low => 0.06,
            Quality::Balanced => 0.12,
            Quality::High => 0.20,
            Quality::Ultra => 0.34,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Record,
    Edit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FolderKind {
    Recordings,
    Screenshots,
}

enum Task {
    Idle,
    Countdown { secs_left: u8, deadline: Instant },
    Starting(Receiver<Result<ActiveRecording, String>>),
    Stopping(Receiver<Result<PathBuf, String>>),
    PickingFolder(Receiver<Option<PathBuf>>, FolderKind),
    Screenshot(Receiver<Result<PathBuf, String>>),
}

struct RegionPicker {
    viewport_id: egui::ViewportId,
    monitor: DisplaySource,
    start: Option<egui::Pos2>,
    current: Option<egui::Pos2>,
    result: Option<Region>,
    cancelled: bool,
    preview_rx: Option<Receiver<Result<PreviewFrame, String>>>,
    preview_texture: Option<egui::TextureHandle>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
#[serde(default)]
struct Prefs {
    fps: u32,
    quality: u8,
    codec: u8,
    system_audio: bool,
    microphone: bool,
    capture_cursor: bool,
    countdown_secs: u8,
    minimize_on_record: bool,
    hotkeys_enabled: bool,
    output_dir: String,
    screenshot_dir: String,
    target_mode: u8,
    monitor_index: usize,
    region_enabled: bool,
    region_x: u32,
    region_y: u32,
    region_width: u32,
    region_height: u32,
    max_duration_secs: u64,
    webcam_enabled: bool,
    webcam_device: usize,
    webcam_size: u8,
    webcam_corner: u8,
    webcam_mirror: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            fps: 60,
            quality: 1,
            codec: 0,
            system_audio: true,
            microphone: false,
            capture_cursor: true,
            countdown_secs: 3,
            minimize_on_record: true,
            hotkeys_enabled: true,
            output_dir: util::recordings_dir().to_string_lossy().to_string(),
            screenshot_dir: util::screenshots_dir().to_string_lossy().to_string(),
            target_mode: 0,
            monitor_index: 0,
            region_enabled: false,
            region_x: 0,
            region_y: 0,
            region_width: 0,
            region_height: 0,
            max_duration_secs: 0,
            webcam_enabled: false,
            webcam_device: 0,
            webcam_size: 22,
            webcam_corner: 3,
            webcam_mirror: true,
        }
    }
}

pub struct App {
    caps: Capabilities,
    page: Page,
    editor: Editor,

    monitors: Vec<DisplaySource>,
    windows: Vec<WindowSource>,
    mode: TargetMode,
    selected_monitor: usize,
    selected_window: usize,
    fps: u32,
    quality: Quality,
    codec: Codec,
    system_audio: bool,
    microphone: bool,
    capture_cursor: bool,
    countdown_secs: u8,
    minimize_on_record: bool,
    hotkeys_enabled: bool,
    output_dir: String,
    screenshot_dir: String,

    region_enabled: bool,
    region_x: u32,
    region_y: u32,
    region_width: u32,
    region_height: u32,
    region_picker: Option<RegionPicker>,

    max_duration_secs: u64,

    webcam_enabled: bool,
    webcam_devices: Vec<String>,
    webcam_device: usize,
    webcam_size: u8,
    webcam_corner: Corner,
    webcam_mirror: bool,

    history: Vec<HistoryEntry>,
    show_all_history: bool,
    pending_delete: Option<String>,
    pending_duration: Option<f64>,

    recording: Option<ActiveRecording>,
    task: Task,
    error: Option<String>,
    saved: Option<PathBuf>,

    confirm_quit: bool,
    quit_after_stop: bool,
    allow_close: bool,

    fps_mark: Instant,
    fps_frames: u64,
    display_fps: f32,

    saved_prefs: Option<Prefs>,
    window_title: String,

    logo_texture: egui::TextureHandle,
    preview_tex: Option<egui::TextureHandle>,
    preview_rx: Option<Receiver<Result<PreviewFrame, String>>>,
    preview_due: Instant,
    preview_key: String,
    titlebar_themed: bool,
    bar_excluded: bool,
    /// Minimize the main window once the recording bar is on screen.
    minimize_pending: Option<Instant>,
    hotkeys: Option<HotkeyListener>,
    hotkey_error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);

        let prefs = load_prefs();
        let caps = backend::capabilities();
        let logo_texture = load_logo_texture(&cc.egui_ctx);

        let mut app = Self {
            page: Page::Record,
            editor: Editor::new(&cc.egui_ctx),
            monitors: Vec::new(),
            windows: Vec::new(),
            mode: if prefs.target_mode == 1 && caps.window_capture {
                TargetMode::Window
            } else {
                TargetMode::Monitor
            },
            selected_monitor: prefs.monitor_index,
            selected_window: 0,
            fps: match prefs.fps {
                24 | 30 | 60 | 120 => prefs.fps,
                _ => 60,
            },
            quality: match prefs.quality {
                0 => Quality::Low,
                2 => Quality::High,
                3 => Quality::Ultra,
                _ => Quality::Balanced,
            },
            codec: if prefs.codec == 1 && caps.hevc {
                Codec::Hevc
            } else {
                Codec::H264
            },
            system_audio: prefs.system_audio,
            microphone: prefs.microphone,
            capture_cursor: prefs.capture_cursor,
            countdown_secs: prefs.countdown_secs.min(10),
            minimize_on_record: prefs.minimize_on_record,
            hotkeys_enabled: prefs.hotkeys_enabled,
            output_dir: non_empty_or(prefs.output_dir, util::recordings_dir),
            screenshot_dir: non_empty_or(prefs.screenshot_dir, util::screenshots_dir),
            region_enabled: prefs.region_enabled && caps.region_capture,
            region_x: prefs.region_x,
            region_y: prefs.region_y,
            region_width: prefs.region_width,
            region_height: prefs.region_height,
            region_picker: None,
            max_duration_secs: prefs.max_duration_secs,
            webcam_enabled: prefs.webcam_enabled,
            webcam_devices: Vec::new(),
            webcam_device: prefs.webcam_device,
            webcam_size: prefs.webcam_size.clamp(8, 60),
            webcam_corner: Corner::from_index(prefs.webcam_corner),
            webcam_mirror: prefs.webcam_mirror,
            history: history::load(),
            show_all_history: false,
            pending_delete: None,
            pending_duration: None,
            recording: None,
            task: Task::Idle,
            error: None,
            saved: None,
            confirm_quit: false,
            quit_after_stop: false,
            allow_close: false,
            fps_mark: Instant::now(),
            fps_frames: 0,
            display_fps: 0.0,
            saved_prefs: None,
            window_title: crate::APP_NAME.to_string(),
            logo_texture,
            preview_tex: None,
            preview_rx: None,
            preview_due: Instant::now(),
            preview_key: String::new(),
            titlebar_themed: false,
            bar_excluded: false,
            minimize_pending: None,
            hotkeys: None,
            hotkey_error: None,
            caps,
        };

        app.refresh_sources();
        app.refresh_webcams();
        app.sync_hotkeys(&cc.egui_ctx);
        app.saved_prefs = Some(app.prefs());
        app
    }

    // -----------------------------------------------------------------
    // State helpers
    // -----------------------------------------------------------------

    fn is_busy(&self) -> bool {
        self.recording.is_some()
            || matches!(
                self.task,
                Task::Starting(_) | Task::Stopping(_) | Task::Screenshot(_)
            )
    }

    fn setup_needed(&self) -> bool {
        !self.caps.missing.is_empty()
    }

    fn hotkeys_active(&self) -> bool {
        self.hotkeys.is_some()
    }

    fn recheck_setup(&mut self, ctx: &egui::Context) {
        self.caps = backend::capabilities();
        if !self.caps.hevc {
            self.codec = Codec::H264;
        }
        if !self.caps.region_capture {
            self.region_enabled = false;
        }
        self.refresh_sources();
        self.refresh_webcams();
        self.sync_hotkeys(ctx);
    }

    fn sync_hotkeys(&mut self, ctx: &egui::Context) {
        let wanted = self.hotkeys_enabled && self.caps.global_hotkeys;
        if wanted && self.hotkeys.is_none() && self.hotkey_error.is_none() {
            match HotkeyListener::new(ctx.clone()) {
                Ok(listener) => self.hotkeys = Some(listener),
                Err(err) => self.hotkey_error = Some(err),
            }
        } else if !wanted {
            self.hotkeys = None;
            self.hotkey_error = None;
        }
    }

    fn refresh_sources(&mut self) {
        self.monitors = backend::list_displays();
        self.refresh_windows();
        if self.selected_monitor >= self.monitors.len() {
            self.selected_monitor = self.monitors.iter().position(|m| m.primary).unwrap_or(0);
        }
    }

    fn refresh_windows(&mut self) {
        let previous = self.windows.get(self.selected_window).map(|w| w.id);
        self.windows = backend::list_windows();
        self.selected_window = previous
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .unwrap_or(0);
    }

    fn refresh_webcams(&mut self) {
        self.webcam_devices = backend::list_cameras();
        if self.webcam_device >= self.webcam_devices.len() {
            self.webcam_device = 0;
        }
    }

    fn current_target(&self) -> Option<CaptureTarget> {
        if self.caps.portal_source {
            return Some(CaptureTarget::Portal {
                window: self.mode == TargetMode::Window,
            });
        }
        match self.mode {
            TargetMode::Monitor => self
                .monitors
                .get(self.selected_monitor)
                .cloned()
                .map(CaptureTarget::Display),
            TargetMode::Window => self
                .windows
                .get(self.selected_window)
                .cloned()
                .map(CaptureTarget::Window),
        }
    }

    fn region_active(&self) -> bool {
        self.caps.region_capture && self.mode == TargetMode::Monitor && self.region_enabled
    }

    fn current_region(&self) -> Region {
        Region {
            x: self.region_x,
            y: self.region_y,
            width: self.region_width,
            height: self.region_height,
        }
    }

    fn output_dimensions(&self) -> Option<(u32, u32)> {
        if self.caps.portal_source {
            return None;
        }
        let target = self.current_target()?;
        let (w, h) = (target.width(), target.height());
        if self.region_active() {
            if let Some(region) = self.current_region().normalized(w, h) {
                return Some((region.width, region.height));
            }
        }
        (w > 0 && h > 0).then_some((w & !1, h & !1))
    }

    fn estimated_bitrate(&self) -> u32 {
        let (w, h) = self.output_dimensions().unwrap_or((1920, 1080));
        let raw = w.max(2) as f32 * h.max(2) as f32 * self.fps as f32 * self.quality.bpp();
        (raw as u64).clamp(4_000_000, 120_000_000) as u32
    }

    fn build_options(&self) -> Result<RecordingOptions, String> {
        let target = self
            .current_target()
            .ok_or_else(|| "Select a display or window to record.".to_string())?;

        let region = if self.region_active() {
            Some(
                self.current_region()
                    .normalized(target.width(), target.height())
                    .ok_or_else(|| {
                        "The selected region is invalid. Select a region on screen or turn off \
                         \"Record a region\"."
                            .to_string()
                    })?,
            )
        } else {
            None
        };

        let webcam = if self.webcam_enabled && !self.webcam_devices.is_empty() {
            Some(WebcamOptions {
                device_index: self.webcam_device.min(self.webcam_devices.len() - 1),
                size_pct: self.webcam_size,
                corner: self.webcam_corner,
                mirror: self.webcam_mirror,
            })
        } else {
            None
        };

        let dir = folder_or(&self.output_dir, util::recordings_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create the recordings folder {}: {e}", dir.display()))?;

        Ok(RecordingOptions {
            target,
            output: util::timestamped_path(&dir, "mp4"),
            fps: self.fps,
            bitrate: self.estimated_bitrate(),
            codec: self.codec,
            system_audio: self.system_audio && self.caps.system_audio,
            microphone: self.microphone,
            capture_cursor: self.capture_cursor,
            region,
            webcam,
        })
    }

    // -----------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------

    fn request_start(&mut self) {
        if self.page == Page::Edit {
            self.editor.stop();
            self.page = Page::Record;
        }
        if self.setup_needed() {
            self.error = Some("Finish the setup shown above before recording.".into());
            return;
        }
        if let Err(err) = self.build_options() {
            self.error = Some(err);
            return;
        }
        self.error = None;
        self.saved = None;
        if self.countdown_secs > 0 {
            self.task = Task::Countdown {
                secs_left: self.countdown_secs,
                deadline: Instant::now() + Duration::from_secs(1),
            };
        } else {
            self.begin_start();
        }
    }

    fn begin_start(&mut self) {
        let options = match self.build_options() {
            Ok(options) => options,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("scroder-start".to_string())
            .spawn(move || {
                let result = backend::start(options).map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            })
            .ok();

        self.task = Task::Starting(rx);
    }

    fn begin_stop(&mut self) {
        if let Some(recording) = self.recording.take() {
            self.pending_duration = Some(recording.elapsed().as_secs_f64());
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("scroder-stop".to_string())
                .spawn(move || {
                    let result = recording.stop().map_err(|e| format!("{e:#}"));
                    let _ = tx.send(result);
                })
                .ok();
            self.task = Task::Stopping(rx);
        }
    }

    fn toggle_pause(&mut self) {
        if let Some(recording) = &self.recording {
            if recording.is_paused() {
                recording.resume();
            } else {
                recording.pause();
            }
        }
    }

    fn take_screenshot(&mut self) {
        if !matches!(self.task, Task::Idle) {
            return;
        }
        if self.setup_needed() {
            self.error = Some("Finish the setup shown above before taking screenshots.".into());
            return;
        }
        let Some(target) = self.current_target() else {
            self.error = Some("Select a display or window first.".to_string());
            return;
        };

        let region = if self.region_active() {
            self.current_region()
                .normalized(target.width(), target.height())
        } else {
            None
        };

        let dir = folder_or(&self.screenshot_dir, util::screenshots_dir);
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("Cannot create the screenshots folder: {err}"));
            return;
        }
        let path = util::timestamped_path(&dir, "png");

        self.error = None;

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("scroder-screenshot".to_string())
            .spawn(move || {
                let result =
                    backend::screenshot(&target, region, path).map_err(|err| format!("{err:#}"));
                let _ = tx.send(result);
            })
            .ok();

        // While recording, the task slot is idle (the recording lives in
        // `self.recording`), so this never replaces another task.
        self.task = Task::Screenshot(rx);
    }

    fn finish_screenshot(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => {
                self.history = history::add(
                    self.history.clone(),
                    HistoryEntry::new(&path, "screenshot", 0.0),
                );
                self.saved = Some(path);
            }
            Err(err) => self.error = Some(format!("Screenshot failed: {err}")),
        }
    }

    fn browse_folder(&mut self, kind: FolderKind) {
        let current = PathBuf::from(match kind {
            FolderKind::Recordings => self.output_dir.trim().to_string(),
            FolderKind::Screenshots => self.screenshot_dir.trim().to_string(),
        });
        let title = match kind {
            FolderKind::Recordings => "Choose where to save recordings",
            FolderKind::Screenshots => "Choose where to save screenshots",
        };

        if shell::PICKER_ON_MAIN_THREAD {
            if let Some(path) = shell::pick_folder(&current, title) {
                self.set_folder(kind, path);
            }
            return;
        }

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("scroder-browse".to_string())
            .spawn(move || {
                let _ = tx.send(shell::pick_folder(&current, title));
            })
            .ok();
        self.task = Task::PickingFolder(rx, kind);
    }

    fn set_folder(&mut self, kind: FolderKind, path: PathBuf) {
        let text = path.to_string_lossy().to_string();
        match kind {
            FolderKind::Recordings => self.output_dir = text,
            FolderKind::Screenshots => self.screenshot_dir = text,
        }
    }

    fn open_folder(&mut self, folder: &str, fallback: fn() -> PathBuf) {
        let dir = folder_or(folder, fallback);
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("Cannot create {}: {err}", dir.display()));
            return;
        }
        if let Err(err) = shell::open_path(&dir) {
            self.error = Some(format!("{err:#}"));
        }
    }

    /// Opens or reveals a capture, dropping it from the list if it is gone.
    fn open_capture(&mut self, path: &Path, reveal: bool) {
        if !path.exists() {
            let key = path.to_string_lossy().to_string();
            self.history.retain(|entry| entry.path != key);
            history::save(&self.history);
            if self.saved.as_deref() == Some(path) {
                self.saved = None;
            }
            self.error = Some(format!(
                "{} was moved or deleted, so it was removed from Recent.",
                file_name(path)
            ));
            return;
        }
        let result = if reveal {
            shell::reveal_path(path)
        } else {
            shell::open_path(path)
        };
        if let Err(err) = result {
            self.error = Some(format!("{err:#}"));
        }
    }

    fn edit_capture(&mut self, path: &Path, ctx: &egui::Context) {
        if !path.exists() {
            self.open_capture(path, false);
            return;
        }
        self.editor.open(path.to_path_buf(), true, ctx);
        self.page = Page::Edit;
    }

    fn start_region_picker(&mut self) {
        let Some(monitor) = self.monitors.get(self.selected_monitor).cloned() else {
            self.error = Some("Select a display first.".to_string());
            return;
        };
        self.error = None;

        let (tx, rx) = mpsc::channel();
        let target = CaptureTarget::Display(monitor.clone());
        std::thread::spawn(move || {
            let result = backend::grab(&target).map_err(|err| err.to_string());
            let _ = tx.send(result);
        });

        self.region_picker = Some(RegionPicker {
            viewport_id: egui::ViewportId::from_hash_of("scroder-region-picker"),
            monitor,
            start: None,
            current: None,
            result: None,
            cancelled: false,
            preview_rx: Some(rx),
            preview_texture: None,
        });
    }

    fn drive_region_picker(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.region_picker.take() else {
            return;
        };

        if picker.preview_texture.is_none() {
            let received = picker.preview_rx.as_ref().map(|rx| rx.try_recv());
            match received {
                Some(Ok(Ok(frame))) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgba,
                    );
                    picker.preview_texture = Some(ctx.load_texture(
                        "region-picker-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                    picker.preview_rx = None;
                }
                Some(Ok(Err(_))) | Some(Err(TryRecvError::Disconnected)) => {
                    picker.preview_rx = None;
                }
                _ => {}
            }
        }

        let (px, py) = picker.monitor.logical_position();
        let (sw, sh) = picker.monitor.logical_size();
        let builder = egui::ViewportBuilder::default()
            .with_title("Select recording region")
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_transparent(true)
            .with_icon(window_icon())
            .with_position(egui::pos2(px, py))
            .with_inner_size([sw, sh]);

        let mut finished = false;

        ctx.show_viewport_immediate(picker.viewport_id, builder, |ui, _class| {
            let ctx = ui.ctx().clone();
            let screen = ctx.content_rect();
            ctx.set_cursor_icon(CursorIcon::Crosshair);

            if ctx.input(|i| i.viewport().close_requested())
                || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                || ctx.input(|i| i.pointer.secondary_clicked())
            {
                picker.cancelled = true;
            }

            let pointer = ctx.input(|i| i.pointer.interact_pos());
            let primary_down = ctx.input(|i| i.pointer.primary_down());
            let primary_released = ctx.input(|i| i.pointer.primary_released());

            if primary_down {
                if picker.start.is_none() {
                    picker.start = pointer;
                }
                if let Some(pos) = pointer {
                    picker.current = Some(pos);
                }
            }

            let to_pixels = |rect: Rect| -> Region {
                let sx = picker.monitor.width as f32 / screen.width().max(1.0);
                let sy = picker.monitor.height as f32 / screen.height().max(1.0);
                Region {
                    x: ((rect.min.x - screen.min.x) * sx).round().max(0.0) as u32,
                    y: ((rect.min.y - screen.min.y) * sy).round().max(0.0) as u32,
                    width: (rect.width() * sx).round().max(2.0) as u32,
                    height: (rect.height() * sy).round().max(2.0) as u32,
                }
            };

            if primary_released && picker.start.is_some() {
                if let (Some(a), Some(b)) = (picker.start, picker.current) {
                    let rect = Rect::from_two_pos(a, b).intersect(screen);
                    if rect.width() > 8.0 && rect.height() > 8.0 {
                        let region = to_pixels(rect);
                        picker.result = Some(
                            region
                                .normalized(picker.monitor.width, picker.monitor.height)
                                .unwrap_or(region),
                        );
                    }
                }
                finished = true;
            }

            if picker.cancelled {
                finished = true;
            }

            let selection = match (picker.start, picker.current) {
                (Some(a), Some(b)) => Some(Rect::from_two_pos(a, b).intersect(screen)),
                _ => None,
            };

            let painter = ui.painter();
            if let Some(texture) = &picker.preview_texture {
                painter.image(
                    texture.id(),
                    screen,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            let dim = Color32::from_black_alpha(120);
            let accent = theme::DARK.accent_hi;

            if let Some(sel) = selection.filter(|s| s.width() > 0.0 && s.height() > 0.0) {
                for band in [
                    Rect::from_min_max(screen.min, pos2(screen.max.x, sel.min.y)),
                    Rect::from_min_max(pos2(screen.min.x, sel.max.y), screen.max),
                    Rect::from_min_max(pos2(screen.min.x, sel.min.y), pos2(sel.min.x, sel.max.y)),
                    Rect::from_min_max(pos2(sel.max.x, sel.min.y), pos2(screen.max.x, sel.max.y)),
                ] {
                    painter.rect_filled(band, 0.0, dim);
                }
                painter.rect_stroke(
                    sel,
                    CornerRadius::same(3),
                    Stroke::new(2.0, accent),
                    StrokeKind::Inside,
                );
                let pixels = to_pixels(sel);
                let label = format!("{} × {}", pixels.width, pixels.height);
                let galley = painter.layout_no_wrap(label, FontId::monospace(13.0), Color32::WHITE);
                let tag = Rect::from_min_size(
                    pos2(
                        sel.min.x,
                        (sel.min.y - galley.size().y - 10.0).max(screen.min.y),
                    ),
                    galley.size() + vec2(12.0, 6.0),
                );
                painter.rect_filled(tag, CornerRadius::same(3), accent);
                painter.galley(tag.min + vec2(6.0, 3.0), galley, Color32::WHITE);
            } else {
                painter.rect_filled(screen, 0.0, dim);
            }

            let hint = if picker.preview_texture.is_none() && picker.preview_rx.is_some() {
                "Preparing preview…   Drag to select the recording area   ·   Esc to cancel"
            } else {
                "Drag to select the recording area   ·   Esc or right-click to cancel"
            };
            let galley = painter.layout_no_wrap(
                hint.to_string(),
                FontId::proportional(15.5),
                Color32::WHITE,
            );
            let pill_rect = Rect::from_center_size(
                pos2(screen.center().x, screen.min.y + 40.0),
                galley.size() + vec2(28.0, 14.0),
            );
            painter.rect_filled(
                pill_rect,
                CornerRadius::same(6),
                Color32::from_black_alpha(190),
            );
            painter.galley(
                pill_rect.center() - galley.size() / 2.0,
                galley,
                Color32::WHITE,
            );

            if finished {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint();
            }
        });

        if finished {
            if let Some(region) = picker.result {
                self.region_enabled = true;
                self.region_x = region.x;
                self.region_y = region.y;
                self.region_width = region.width;
                self.region_height = region.height;
            }
        } else {
            self.region_picker = Some(picker);
        }
    }

    fn handle_hotkey(&mut self, action: HotkeyAction) {
        match action {
            HotkeyAction::ToggleRecord => {
                if self.recording.is_some() {
                    self.begin_stop();
                } else if matches!(self.task, Task::Countdown { .. }) {
                    self.task = Task::Idle;
                } else if matches!(self.task, Task::Idle) {
                    self.request_start();
                }
            }
            HotkeyAction::TogglePause => self.toggle_pause(),
            HotkeyAction::Screenshot => self.take_screenshot(),
        }
    }

    fn poll_tasks(&mut self, ctx: &egui::Context) {
        self.sync_hotkeys(ctx);

        let mut actions: Vec<HotkeyAction> = self
            .hotkeys
            .as_ref()
            .map(HotkeyListener::poll)
            .unwrap_or_default();

        // Without system-wide shortcuts, the keys still work inside the window.
        if !self.hotkeys_active() && self.region_picker.is_none() && !self.confirm_quit {
            ctx.input(|i| {
                for (key, action) in [
                    (egui::Key::F9, HotkeyAction::ToggleRecord),
                    (egui::Key::F10, HotkeyAction::TogglePause),
                    (egui::Key::F11, HotkeyAction::Screenshot),
                ] {
                    if i.key_pressed(key) {
                        actions.push(action);
                    }
                }
            });
        }
        if matches!(self.task, Task::Countdown { .. })
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.task = Task::Idle;
        }

        for action in actions {
            self.handle_hotkey(action);
        }

        if let Some(recording) = &self.recording {
            let reached = self.max_duration_secs > 0
                && recording.elapsed() >= Duration::from_secs(self.max_duration_secs);
            if !recording.is_capture_alive() {
                self.error = Some(
                    recording
                        .error()
                        .unwrap_or_else(|| "The capture stopped unexpectedly.".into()),
                );
                self.begin_stop();
            } else if reached {
                self.begin_stop();
            }
        }

        let current = std::mem::replace(&mut self.task, Task::Idle);

        match current {
            Task::Countdown {
                mut secs_left,
                deadline,
            } => {
                if Instant::now() >= deadline {
                    secs_left = secs_left.saturating_sub(1);
                    if secs_left == 0 {
                        self.begin_start();
                    } else {
                        self.task = Task::Countdown {
                            secs_left,
                            deadline: deadline + Duration::from_secs(1),
                        };
                    }
                } else {
                    self.task = Task::Countdown {
                        secs_left,
                        deadline,
                    };
                }
            }
            Task::Starting(rx) => match rx.try_recv() {
                Ok(Ok(recording)) => {
                    self.recording = Some(recording);
                    self.fps_mark = Instant::now();
                    self.fps_frames = 0;
                    self.display_fps = 0.0;

                    if self.minimize_on_record {
                        if cfg!(windows) {
                            // Wait for the recording bar to appear first: once the
                            // main window is minimized, eframe would never paint it.
                            self.minimize_pending = Some(Instant::now());
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    }
                }
                Ok(Err(err)) => self.error = Some(err),
                Err(TryRecvError::Empty) => self.task = Task::Starting(rx),
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("Recording failed to start.".to_string());
                }
            },
            Task::Stopping(rx) => match rx.try_recv() {
                Ok(result) => {
                    let duration = self.pending_duration.take().unwrap_or(0.0);
                    match result {
                        Ok(path) => {
                            self.history = history::add(
                                self.history.clone(),
                                HistoryEntry::new(&path, "video", duration),
                            );
                            self.saved = Some(path);
                        }
                        Err(err) => self.error = Some(err),
                    }
                    if self.quit_after_stop {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else if self.minimize_on_record {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
                Err(TryRecvError::Empty) => self.task = Task::Stopping(rx),
                Err(TryRecvError::Disconnected) => {
                    self.pending_duration = None;
                    self.error = Some("Failed to finalize the recording.".to_string());
                    if self.minimize_on_record {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    }
                }
            },
            Task::PickingFolder(rx, kind) => match rx.try_recv() {
                Ok(Some(path)) => self.set_folder(kind, path),
                Ok(None) | Err(TryRecvError::Disconnected) => {}
                Err(TryRecvError::Empty) => self.task = Task::PickingFolder(rx, kind),
            },
            Task::Screenshot(rx) => match rx.try_recv() {
                Ok(result) => self.finish_screenshot(result),
                Err(TryRecvError::Empty) => self.task = Task::Screenshot(rx),
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("Screenshot failed.".to_string());
                }
            },
            Task::Idle => {}
        }
    }

    fn prefs(&self) -> Prefs {
        Prefs {
            fps: self.fps,
            quality: match self.quality {
                Quality::Low => 0,
                Quality::Balanced => 1,
                Quality::High => 2,
                Quality::Ultra => 3,
            },
            codec: if self.codec == Codec::Hevc { 1 } else { 0 },
            system_audio: self.system_audio,
            microphone: self.microphone,
            capture_cursor: self.capture_cursor,
            countdown_secs: self.countdown_secs,
            minimize_on_record: self.minimize_on_record,
            hotkeys_enabled: self.hotkeys_enabled,
            output_dir: self.output_dir.clone(),
            screenshot_dir: self.screenshot_dir.clone(),
            target_mode: if self.mode == TargetMode::Window {
                1
            } else {
                0
            },
            monitor_index: self.selected_monitor,
            region_enabled: self.region_enabled,
            region_x: self.region_x,
            region_y: self.region_y,
            region_width: self.region_width,
            region_height: self.region_height,
            max_duration_secs: self.max_duration_secs,
            webcam_enabled: self.webcam_enabled,
            webcam_device: self.webcam_device,
            webcam_size: self.webcam_size,
            webcam_corner: self.webcam_corner.index(),
            webcam_mirror: self.webcam_mirror,
        }
    }

    /// Persists settings as soon as they change.
    fn autosave_prefs(&mut self) {
        let prefs = self.prefs();
        if self.saved_prefs.as_ref() == Some(&prefs) {
            return;
        }
        if let (Ok(text), Some(dir)) = (serde_json::to_string_pretty(&prefs), util::config_dir()) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("config.json"), text);
        }
        self.saved_prefs = Some(prefs);
    }

    fn update_stats(&mut self) {
        let Some(recording) = &self.recording else {
            return;
        };
        let frames = recording.frames();
        let now = Instant::now();
        let dt = now.duration_since(self.fps_mark).as_secs_f32();
        if dt >= 0.5 {
            let measured = frames.saturating_sub(self.fps_frames) as f32 / dt;
            self.display_fps = if recording.is_paused() {
                0.0
            } else if self.display_fps <= 0.1 {
                measured
            } else {
                self.display_fps * 0.55 + measured * 0.45
            };
            self.fps_frames = frames;
            self.fps_mark = now;
        }
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = match &self.recording {
            Some(recording) if recording.is_paused() => format!(
                "Paused {} — {}",
                fmt_duration(recording.elapsed()),
                crate::APP_NAME
            ),
            Some(recording) => format!(
                "● Recording {} — {}",
                fmt_duration(recording.elapsed()),
                crate::APP_NAME
            ),
            None => crate::APP_NAME.to_string(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        let active = self.recording.is_some()
            || matches!(self.task, Task::Starting(_) | Task::Stopping(_))
            || self.editor.has_unsaved_edits()
            || self.editor.is_exporting();
        if active {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }
    }

    fn source_summary(&self) -> String {
        match self.current_target() {
            Some(target) => target.label(),
            None => match self.mode {
                TargetMode::Monitor => "No display selected".into(),
                TargetMode::Window => "No window selected".into(),
            },
        }
    }

    // -----------------------------------------------------------------
    // Chrome
    // -----------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let frame = egui::Frame::new()
            .fill(p.panel_top)
            .inner_margin(Margin::symmetric(GUTTER as i8, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(&self.logo_texture)
                            .fit_to_exact_size(Vec2::splat(34.0))
                            .corner_radius(CornerRadius::same(4)),
                    );
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add_space(1.0);
                        ui.label(
                            RichText::new(crate::APP_NAME)
                                .size(17.0)
                                .family(crate::theme::bold())
                                .color(p.text),
                        );
                        ui.label(
                            RichText::new("Record · Edit · Share")
                                .size(12.0)
                                .color(p.muted),
                        );
                    });
                    ui.add_space(28.0);
                    let before = self.page;
                    segmented_sized(
                        ui,
                        "page",
                        &mut self.page,
                        &[
                            (Page::Record, Some(Icon::Record), "Record"),
                            (Page::Edit, Some(Icon::Edit), "Edit"),
                        ],
                        vec2(214.0, 34.0),
                    );
                    if before == Page::Edit && self.page != Page::Edit {
                        self.editor.stop();
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        pill(
                            ui,
                            &format!("v{}", crate::VERSION),
                            p.text_soft,
                            p.surface_elevated,
                            p.border,
                        );
                    });
                });
            });

        let rect = frame.response.rect;
        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, p.border_soft),
        );
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let p = pal();
        let frame = egui::Frame::new()
            .fill(p.panel_top)
            .inner_margin(Margin::symmetric(GUTTER as i8, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let paused = self.recording.as_ref().is_some_and(|r| r.is_paused());
                    let (label, color) = if self.setup_needed() {
                        ("Setup needed", p.caution)
                    } else if self.recording.is_some() && paused {
                        ("Paused", p.caution)
                    } else if self.recording.is_some() {
                        ("Recording", p.danger)
                    } else if self.error.is_some() {
                        ("Attention needed", p.danger)
                    } else if !matches!(self.task, Task::Idle) {
                        ("Working", p.accent)
                    } else {
                        ("Ready", p.success)
                    };

                    status_dot(ui, color);
                    ui.add_space(2.0);
                    ui.label(RichText::new(label).size(12.0).color(p.text_soft));
                    ui.label(RichText::new("·").size(12.0).color(p.muted_dim));
                    ui.label(RichText::new(self.caps.platform).size(12.0).color(p.muted));

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let hint = if self.hotkeys_active() {
                            "F9 record  ·  F10 pause  ·  F11 screenshot"
                        } else if self.caps.global_hotkeys {
                            "Global shortcuts off  ·  F9 / F10 / F11 work in this window"
                        } else {
                            "F9 record  ·  F10 pause  ·  F11 screenshot  (in this window)"
                        };
                        ui.label(RichText::new(hint).size(11.5).color(p.muted_dim));
                    });
                });
            });

        let rect = frame.response.rect;
        ui.painter().line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, p.border_soft),
        );
    }

    // -----------------------------------------------------------------
    // Settings column
    // -----------------------------------------------------------------

    /// Keeps a small live thumbnail of the selected source for the screen tile.
    fn drive_preview(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.preview_rx {
            match rx.try_recv() {
                Ok(Ok(frame)) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgba,
                    );
                    match &mut self.preview_tex {
                        Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            self.preview_tex = Some(ctx.load_texture(
                                "source-preview",
                                image,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                    }
                    self.preview_rx = None;
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => self.preview_rx = None,
                Err(TryRecvError::Empty) => {}
            }
            return;
        }
        if self.recording.is_some()
            || !matches!(self.task, Task::Idle)
            || self.caps.portal_source
            || self.setup_needed()
            || self.region_picker.is_some()
            || !ctx.input(|i| i.focused)
        {
            return;
        }
        let Some(target) = self.current_target() else {
            self.preview_tex = None;
            return;
        };
        let key = target.label();
        let now = Instant::now();
        if key == self.preview_key && now < self.preview_due {
            ctx.request_repaint_after(self.preview_due - now);
            return;
        }
        if key != self.preview_key {
            self.preview_tex = None;
        }
        self.preview_key = key;
        self.preview_due = now + Duration::from_secs(3);
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::Builder::new()
            .name("scroder-thumbnail".into())
            .spawn(move || {
                let result = backend::grab(&target)
                    .map(|frame| downscale(&frame, 480))
                    .map_err(|err| format!("{err:#}"));
                let _ = tx.send(result);
                wake.request_repaint();
            })
            .ok();
        self.preview_rx = Some(rx);
    }

    fn recorder_strip(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let blocked = self.setup_needed();
        let shortcuts = self.hotkeys_active() || !self.caps.global_hotkeys;
        let mut start = false;
        egui::Frame::new()
            .fill(p.panel_top)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(theme::RADIUS_PANEL))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let height = 154.0;
                let gap = 10.0;
                let rec_w = 104.0;
                let unit = ((ui.available_width() - rec_w - gap * 4.0) / 4.35).max(84.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    self.screen_tile(ui, vec2(unit * 1.35, height));

                    let camera = if self.webcam_devices.is_empty() {
                        "No camera".to_string()
                    } else {
                        self.webcam_devices
                            .get(self.webcam_device)
                            .cloned()
                            .unwrap_or_else(|| "Camera".into())
                    };
                    source_tile(
                        ui,
                        "tile_camera",
                        vec2(unit, height),
                        Icon::Camera,
                        &camera,
                        &mut self.webcam_enabled,
                        !self.webcam_devices.is_empty(),
                        "Overlay your webcam on the recording",
                    );
                    source_tile(
                        ui,
                        "tile_mic",
                        vec2(unit, height),
                        Icon::Microphone,
                        "Microphone",
                        &mut self.microphone,
                        true,
                        "Record your voice",
                    );
                    let note = self
                        .caps
                        .system_audio_note
                        .clone()
                        .unwrap_or_else(|| "Record what plays through your speakers".into());
                    let mut system = self.system_audio && self.caps.system_audio;
                    source_tile(
                        ui,
                        "tile_system",
                        vec2(unit, height),
                        Icon::Speaker,
                        "System Audio",
                        &mut system,
                        self.caps.system_audio,
                        &note,
                    );
                    if self.caps.system_audio {
                        self.system_audio = system;
                    }

                    // Rec column: button + "Start [F9]", centred on the tiles.
                    let (column, _) = ui.allocate_exact_size(vec2(rec_w, height), Sense::hover());
                    let diameter = 84.0;
                    let caption_h = 22.0;
                    let caption_gap = 12.0;
                    let top = column.top() + (height - diameter - caption_gap - caption_h) / 2.0;
                    let button_rect = Rect::from_center_size(
                        pos2(column.center().x, top + diameter / 2.0),
                        Vec2::splat(diameter),
                    );
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(button_rect));
                    if record_button(&mut child, diameter, !blocked, shortcuts).clicked() {
                        start = true;
                    }
                    let caption = Rect::from_min_size(
                        pos2(column.left(), button_rect.bottom() + caption_gap),
                        vec2(rec_w, caption_h),
                    );
                    let painter = ui.painter();
                    let label = painter.layout_no_wrap(
                        "Start".to_string(),
                        FontId::new(13.5, theme::bold()),
                        if blocked { p.muted_dim } else { p.text },
                    );
                    let show_key = shortcuts && !blocked;
                    let key = painter.layout_no_wrap(
                        "F9".to_string(),
                        FontId::monospace(10.0),
                        p.text_soft,
                    );
                    let key_w = key.size().x + 12.0;
                    let total = label.size().x + if show_key { 8.0 + key_w } else { 0.0 };
                    let x = caption.center().x - total / 2.0;
                    painter.galley(
                        pos2(x, caption.center().y - label.size().y / 2.0),
                        label.clone(),
                        p.text,
                    );
                    if show_key {
                        let cap = Rect::from_min_size(
                            pos2(x + label.size().x + 8.0, caption.center().y - 10.0),
                            vec2(key_w, 20.0),
                        );
                        painter.rect(
                            cap,
                            CornerRadius::same(theme::RADIUS),
                            p.surface_elevated,
                            Stroke::new(1.0, p.border_hi),
                            StrokeKind::Inside,
                        );
                        painter.galley(cap.center() - key.size() / 2.0, key, p.text_soft);
                    }
                });
            });
        if start {
            self.request_start();
        }
    }

    fn screen_tile(&mut self, ui: &mut egui::Ui, size: Vec2) {
        let p = pal();
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);
        let bar_h = 40.0;
        let top = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - bar_h));
        let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - bar_h), rect.max);
        painter.rect(
            rect,
            CornerRadius::same(theme::RADIUS_PANEL),
            p.surface,
            Stroke::new(1.0, p.border_soft),
            StrokeKind::Inside,
        );
        painter.line_segment(
            [
                pos2(rect.left() + 1.0, bar.top()),
                pos2(rect.right() - 1.0, bar.top()),
            ],
            Stroke::new(1.0, p.border_soft),
        );
        painter.rect_filled(
            top.shrink(6.0),
            CornerRadius::same(theme::RADIUS),
            Color32::from_rgb(12, 13, 17),
        );

        let area = top.shrink(8.0);
        match (&self.preview_tex, self.caps.portal_source) {
            (Some(texture), false) => {
                let image = texture.size_vec2();
                let scale = (area.width() / image.x).min(area.height() / image.y);
                let shown = Rect::from_center_size(area.center(), image * scale);
                painter.image(
                    texture.id(),
                    shown,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                if self.region_active() {
                    if let Some(target) = self.current_target() {
                        if let Some(r) = self
                            .current_region()
                            .normalized(target.width(), target.height())
                        {
                            let sx = shown.width() / target.width().max(1) as f32;
                            let sy = shown.height() / target.height().max(1) as f32;
                            let region = Rect::from_min_size(
                                shown.min + vec2(r.x as f32 * sx, r.y as f32 * sy),
                                vec2(r.width as f32 * sx, r.height as f32 * sy),
                            );
                            painter.rect_filled(shown, 0.0, Color32::from_black_alpha(90));
                            painter.image(
                                texture.id(),
                                region,
                                Rect::from_min_max(
                                    pos2(
                                        (region.min.x - shown.min.x) / shown.width(),
                                        (region.min.y - shown.min.y) / shown.height(),
                                    ),
                                    pos2(
                                        (region.max.x - shown.min.x) / shown.width(),
                                        (region.max.y - shown.min.y) / shown.height(),
                                    ),
                                ),
                                Color32::WHITE,
                            );
                            painter.rect_stroke(
                                region,
                                CornerRadius::ZERO,
                                Stroke::new(2.0, p.success),
                                StrokeKind::Outside,
                            );
                        }
                    }
                }
            }
            _ => {
                let icon = if self.mode == TargetMode::Window {
                    Icon::Window
                } else {
                    Icon::Monitor
                };
                icons::paint(
                    &painter,
                    Rect::from_center_size(area.center(), Vec2::splat(42.0)),
                    icon,
                    p.text_soft,
                );
            }
        }

        if let Some((w, h)) = self.output_dimensions() {
            let text = format!("{w} × {h}");
            let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), p.text);
            let chip = Rect::from_min_size(
                pos2(area.left() + 4.0, area.bottom() - galley.size().y - 10.0),
                galley.size() + vec2(12.0, 6.0),
            );
            painter.rect_filled(
                chip,
                CornerRadius::same(theme::RADIUS),
                Color32::from_black_alpha(185),
            );
            painter.galley(chip.min + vec2(6.0, 3.0), galley, p.text);
        }

        icons::paint(
            &painter,
            Rect::from_center_size(pos2(bar.left() + 19.0, bar.center().y), Vec2::splat(18.0)),
            if self.mode == TargetMode::Window {
                Icon::Window
            } else {
                Icon::Monitor
            },
            p.text,
        );
        let label = if self.caps.portal_source {
            if self.mode == TargetMode::Window {
                "Window · chosen when you start".to_string()
            } else {
                "Screen · chosen when you start".to_string()
            }
        } else if self.region_active() {
            "Region".to_string()
        } else {
            match self.current_target() {
                Some(CaptureTarget::Display(d)) => d.name.clone(),
                Some(CaptureTarget::Window(w)) => w.title.clone(),
                _ => "No source".into(),
            }
        };
        let mut job = egui::text::LayoutJob::simple_singleline(
            label,
            FontId::new(13.5, theme::bold()),
            p.text,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(bar.width() - 46.0);
        let galley = painter.layout_job(job);
        painter.galley(
            pos2(bar.left() + 36.0, bar.center().y - galley.size().y / 2.0),
            galley,
            p.text,
        );
        response.on_hover_text("Choose the display, window or region in Capture below");
    }

    fn source_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Monitor, "Capture", "Source and area");
            ui.add_space(10.0);

            let p = pal();
            if self.caps.window_capture {
                let before = self.mode;
                segmented(
                    ui,
                    "target_mode",
                    &mut self.mode,
                    &[
                        (TargetMode::Monitor, Some(Icon::Monitor), "Display"),
                        (TargetMode::Window, Some(Icon::Window), "Window"),
                    ],
                );
                if before != self.mode
                    && self.mode == TargetMode::Window
                    && !self.caps.portal_source
                {
                    self.refresh_windows();
                }
                ui.add_space(10.0);
            }

            if self.caps.portal_source {
                let what = if self.mode == TargetMode::Window {
                    "window"
                } else {
                    "screen"
                };
                info_note(
                    ui,
                    Icon::Info,
                    &format!(
                        "When you start recording, your desktop asks which {what} to share. \
                         Scroder remembers the choice for next time."
                    ),
                    p.text_soft,
                );
                return;
            }

            match self.mode {
                TargetMode::Monitor => {
                    field_label(ui, Some(Icon::Monitor), "Display");
                    let labels: Vec<String> = self.monitors.iter().map(|m| m.label()).collect();
                    let current = labels
                        .get(self.selected_monitor)
                        .cloned()
                        .unwrap_or_else(|| "No displays detected".to_string());
                    let mut refresh = false;
                    picker_row(
                        ui,
                        "monitor",
                        &current,
                        &labels,
                        &mut self.selected_monitor,
                        &mut refresh,
                    );
                    if refresh {
                        self.refresh_sources();
                    }

                    if self.caps.region_capture {
                        ui.add_space(6.0);
                        toggle_row(
                            ui,
                            Icon::Crop,
                            "Record a region",
                            "Limit capture to part of the display",
                            &mut self.region_enabled,
                        );

                        if self.region_enabled {
                            ui.add_space(6.0);
                            self.region_editor(ui);
                        }
                    }
                }
                TargetMode::Window => {
                    field_label(ui, Some(Icon::Window), "Window");
                    let labels: Vec<String> = self.windows.iter().map(|w| w.label()).collect();
                    let current = labels
                        .get(self.selected_window)
                        .cloned()
                        .unwrap_or_else(|| "No windows detected".to_string());
                    let mut refresh = false;
                    picker_row(
                        ui,
                        "window",
                        &current,
                        &labels,
                        &mut self.selected_window,
                        &mut refresh,
                    );
                    if refresh {
                        self.refresh_windows();
                    }
                    if self.windows.is_empty() {
                        ui.add_space(4.0);
                        info_note(
                            ui,
                            Icon::Alert,
                            "No windows found. Open the app you want to record, then press refresh.",
                            p.caution,
                        );
                    }
                }
            }
        });
    }

    fn region_editor(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let (max_w, max_h) = self
            .monitors
            .get(self.selected_monitor)
            .map(|m| (m.width, m.height))
            .unwrap_or((0, 0));

        mask(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                number_field(ui, "X", &mut self.region_x, 0..=max_w);
                number_field(ui, "Y", &mut self.region_y, 0..=max_h);
                number_field(ui, "Width", &mut self.region_width, 2..=max_w.max(2));
                number_field(ui, "Height", &mut self.region_height, 2..=max_h.max(2));
            });

            ui.add_space(6.0);
            if wide_button(
                ui,
                Some(Icon::Crop),
                "Select region on screen",
                ButtonKind::Secondary,
                34.0,
            )
            .clicked()
            {
                self.start_region_picker();
            }

            let valid = self.current_region().normalized(max_w, max_h).is_some();
            if !valid {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "No area selected yet. Select a region on screen or enter its size.",
                    )
                    .size(12.0)
                    .color(p.caution),
                );
            }
        });
    }

    fn video_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            let p = pal();
            section_header(ui, Icon::Sliders, "Video", "Frame rate, quality and codec");
            ui.add_space(10.0);

            field_label(ui, Some(Icon::Timer), "Frame rate");
            segmented(
                ui,
                "fps",
                &mut self.fps,
                &[
                    (24u32, None, "24 fps"),
                    (30, None, "30 fps"),
                    (60, None, "60 fps"),
                    (120, None, "120 fps"),
                ],
            );
            ui.add_space(10.0);

            ui.columns(2, |columns| {
                columns[0].vertical(|ui| {
                    field_label(ui, Some(Icon::Film), "Quality");
                    select_enum(
                        ui,
                        "quality",
                        self.quality.short(),
                        &Quality::ALL.map(|quality| (quality, quality.label())),
                        &mut self.quality,
                    );
                });

                columns[1].vertical(|ui| {
                    field_label(ui, Some(Icon::Chip), "Codec");
                    if self.caps.hevc {
                        select_enum(
                            ui,
                            "codec",
                            self.codec.label(),
                            &[
                                (Codec::H264, "H.264 · most compatible"),
                                (Codec::Hevc, "HEVC · smaller files"),
                            ],
                            &mut self.codec,
                        );
                    } else {
                        ui.add_enabled_ui(false, |ui| {
                            select_enum(
                                ui,
                                "codec",
                                "H.264",
                                &[(Codec::H264, "H.264")],
                                &mut self.codec,
                            );
                        })
                        .response
                        .on_disabled_hover_text("HEVC is not available with this encoder");
                    }
                });
            });

            ui.add_space(10.0);
            let dims = self.output_dimensions();
            let bitrate = util::human_bitrate(self.estimated_bitrate() as u64);
            mask(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Output").size(12.0).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let text = match dims {
                            Some((w, h)) => format!("{w} × {h}"),
                            None => "Size of the shared source".to_string(),
                        };
                        ui.label(
                            RichText::new(text)
                                .size(12.0)
                                .family(crate::theme::bold())
                                .color(p.text_soft),
                        );
                    });
                });
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!(
                        "{} fps · {} · about {bitrate}",
                        self.fps,
                        self.codec.label()
                    ))
                    .size(12.0)
                    .color(p.muted),
                );
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    icons::paint(ui.painter(), rect, Icon::Bolt, p.muted_dim);
                    ui.label(
                        RichText::new(&self.caps.encoder_label)
                            .size(11.5)
                            .color(p.muted_dim),
                    );
                });
            });
        });
    }

    fn webcam_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            let p = pal();
            section_header(
                ui,
                Icon::Camera,
                "Camera overlay",
                "Webcam position and size",
            );
            ui.add_space(8.0);
            mask(ui, |ui| {
                if self.webcam_devices.is_empty() {
                    info_note(
                        ui,
                        Icon::Alert,
                        "No camera detected. Connect a camera, then refresh.",
                        p.caution,
                    );
                    ui.add_space(4.0);
                    if wide_button(
                        ui,
                        Some(Icon::Refresh),
                        "Refresh cameras",
                        ButtonKind::Secondary,
                        32.0,
                    )
                    .clicked()
                    {
                        self.refresh_webcams();
                    }
                    return;
                }

                field_label(ui, Some(Icon::Camera), "Camera");
                let labels: Vec<String> = self
                    .webcam_devices
                    .iter()
                    .map(|n| truncate(n, 48))
                    .collect();
                let current = labels
                    .get(self.webcam_device)
                    .cloned()
                    .unwrap_or_else(|| "Camera".to_string());
                let mut refresh = false;
                picker_row(
                    ui,
                    "webcam_device",
                    &current,
                    &labels,
                    &mut self.webcam_device,
                    &mut refresh,
                );
                if refresh {
                    self.refresh_webcams();
                }

                ui.add_space(8.0);
                field_label(ui, Some(Icon::Sliders), "Size");
                ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0);
                ui.add(
                    egui::Slider::new(&mut self.webcam_size, 8..=60)
                        .suffix("% of height")
                        .show_value(true),
                );

                ui.add_space(8.0);
                field_label(ui, Some(Icon::Corner), "Position");
                segmented(
                    ui,
                    "webcam_corner",
                    &mut self.webcam_corner,
                    &Corner::ALL.map(|corner| (corner, None, corner.label())),
                );

                ui.add_space(4.0);
                toggle_row(
                    ui,
                    Icon::Mirror,
                    "Mirror camera",
                    "Flip the camera image horizontally",
                    &mut self.webcam_mirror,
                );
            });
        });
    }

    fn preferences_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            let p = pal();
            section_header(ui, Icon::Record, "Recording", "Timing and shortcuts");
            ui.add_space(10.0);

            field_label(ui, Some(Icon::Timer), "Countdown before recording");
            segmented(
                ui,
                "countdown",
                &mut self.countdown_secs,
                &[
                    (0u8, None, "Off"),
                    (3, None, "3 s"),
                    (5, None, "5 s"),
                    (10, None, "10 s"),
                ],
            );

            ui.add_space(10.0);
            field_label(ui, Some(Icon::Hourglass), "Stop automatically after");
            segmented(
                ui,
                "max_duration",
                &mut self.max_duration_secs,
                &[
                    (0u64, None, "Never"),
                    (60, None, "1 min"),
                    (300, None, "5 min"),
                    (600, None, "10 min"),
                    (1800, None, "30 min"),
                    (3600, None, "1 h"),
                ],
            );

            ui.add_space(8.0);
            toggle_row(
                ui,
                Icon::Cursor,
                "Mouse cursor",
                "Show the pointer in the recording",
                &mut self.capture_cursor,
            );
            toggle_row(
                ui,
                Icon::Window,
                "Minimize while recording",
                "Keep Scroder out of the capture",
                &mut self.minimize_on_record,
            );

            if self.caps.global_hotkeys {
                toggle_row(
                    ui,
                    Icon::Keyboard,
                    "Global shortcuts",
                    "F9 record · F10 pause · F11 screenshot, from any app",
                    &mut self.hotkeys_enabled,
                );
                if let Some(err) = &self.hotkey_error {
                    info_note(
                        ui,
                        Icon::Alert,
                        &format!("Shortcuts unavailable: {err}. They still work in this window."),
                        p.caution,
                    );
                } else if let Some(note) = &self.caps.hotkey_note {
                    info_note(ui, Icon::Info, note, p.muted);
                }
            } else if let Some(note) = &self.caps.hotkey_note {
                ui.add_space(2.0);
                info_note(ui, Icon::Keyboard, note, p.muted);
            }
        });
    }

    // -----------------------------------------------------------------
    // Control column
    // -----------------------------------------------------------------

    fn setup_card(&mut self, ui: &mut egui::Ui) {
        if self.caps.missing.is_empty() {
            return;
        }
        let p = pal();
        let mut recheck = false;
        egui::Frame::new()
            .fill(p.caution_soft)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, p.caution.gamma_multiply(0.45)))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for (i, missing) in self.caps.missing.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(10.0);
                    }
                    ui.horizontal(|ui| {
                        icon_badge_sized(ui, Icon::Alert, p.caution, p.surface, 28.0);
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&missing.title)
                                .size(13.5)
                                .family(crate::theme::bold())
                                .color(p.text),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new(&missing.detail).size(12.0).color(p.text_soft));
                    if let Some(command) = &missing.install_command {
                        ui.add_space(6.0);
                        command_box(ui, command);
                    }
                }
                ui.add_space(10.0);
                if wide_button(
                    ui,
                    Some(Icon::Refresh),
                    "Check again",
                    ButtonKind::Secondary,
                    34.0,
                )
                .clicked()
                {
                    recheck = true;
                }
            });
        if recheck {
            self.recheck_setup(ui.ctx());
        }
        ui.add_space(12.0);
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let countdown = match &self.task {
            Task::Countdown { secs_left, .. } => Some(*secs_left),
            _ => None,
        };
        let shortcuts = self.hotkeys_active() || !self.caps.global_hotkeys;

        card(ui, |ui| {
            let p = pal();
            if self.recording.is_some() {
                self.recording_view(ui, shortcuts);
            } else if let Some(secs_left) = countdown {
                status_pill(ui, "STARTING", p.caution, p.caution_soft);
                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(secs_left.to_string())
                            .font(FontId::monospace(54.0))
                            .family(crate::theme::bold())
                            .color(p.text),
                    );
                    ui.label(
                        RichText::new("Recording starts shortly")
                            .size(12.5)
                            .color(p.muted),
                    );
                });
                ui.add_space(16.0);
                if wide_button(ui, Some(Icon::Close), "Cancel", ButtonKind::Secondary, 38.0)
                    .on_hover_text("Esc")
                    .clicked()
                {
                    self.task = Task::Idle;
                }
            } else if !matches!(self.task, Task::Idle) {
                let (label, detail) = match &self.task {
                    Task::Starting(_) if self.caps.portal_source => (
                        "Waiting for screen sharing",
                        "Choose what to share in the system dialog",
                    ),
                    Task::Starting(_) => (
                        "Starting the recorder",
                        "Preparing capture and audio devices",
                    ),
                    Task::Stopping(_) => ("Saving the recording", "Writing the video file to disk"),
                    Task::PickingFolder(..) => ("Choosing a folder", "Pick a folder in the dialog"),
                    Task::Screenshot(_) => ("Taking a screenshot", "Saving the current frame"),
                    _ => ("Working", "Please wait"),
                };

                status_pill(ui, "BUSY", p.accent_hi, p.accent_soft);
                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.add(egui::Spinner::new().size(28.0).color(p.accent_hi));
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(label)
                            .size(13.5)
                            .family(crate::theme::bold())
                            .color(p.text),
                    );
                    ui.label(RichText::new(detail).size(12.0).color(p.muted));
                });
                ui.add_space(18.0);
            } else {
                self.idle_view(ui, shortcuts);
            }
        });
    }

    fn idle_view(&mut self, ui: &mut egui::Ui, shortcuts: bool) {
        let p = pal();
        let blocked = self.setup_needed();
        ui.horizontal(|ui| {
            if blocked {
                status_pill(ui, "SETUP NEEDED", p.caution, p.caution_soft);
            } else {
                status_pill(ui, "READY", p.success_text, p.success_soft);
            }
            if shortcuts && !blocked {
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    keycap(ui, "F9");
                });
            }
        });

        ui.add_space(10.0);
        let mut screenshot_clicked = false;
        let hint = if blocked {
            "Complete the setup above to start recording."
        } else if shortcuts {
            "Press rec or F9 to start recording."
        } else {
            "Press rec to start recording."
        };
        ui.label(RichText::new(hint).size(13.0).color(p.text_soft));
        ui.add_space(10.0);

        let output = match self.output_dimensions() {
            Some((w, h)) => format!("{w} × {h} · {} fps", self.fps),
            None => format!("{} fps", self.fps),
        };
        let source = self.source_summary();
        mask(ui, |ui| {
            detail_row(ui, Icon::Monitor, "Source", &source);
            detail_row(ui, Icon::Film, "Output", &output);
            let mut audio = Vec::new();
            if self.system_audio && self.caps.system_audio {
                audio.push("System");
            }
            if self.microphone {
                audio.push("Mic");
            }
            let audio = if audio.is_empty() {
                "Off".to_string()
            } else {
                audio.join(" + ")
            };
            detail_row(ui, Icon::Speaker, "Audio", &audio);
        });

        ui.add_space(12.0);
        ui.add_enabled_ui(!blocked, |ui| {
            if wide_button(
                ui,
                Some(Icon::Screenshot),
                "Take screenshot",
                ButtonKind::Secondary,
                38.0,
            )
            .on_hover_text_if(shortcuts, "F11")
            .clicked()
            {
                screenshot_clicked = true;
            }
        });

        if screenshot_clicked {
            self.take_screenshot();
        }
    }

    fn recording_view(&mut self, ui: &mut egui::Ui, shortcuts: bool) {
        let p = pal();
        let Some(recording) = &self.recording else {
            return;
        };
        let elapsed = recording.elapsed();
        let size = util::human_size(recording.file_size());
        let paused = recording.is_paused();
        let level = recording.audio_level();
        let warning = recording.error();
        let audio_on = (self.system_audio && self.caps.system_audio) || self.microphone;

        ui.horizontal(|ui| {
            if paused {
                status_pill(ui, "PAUSED", p.caution, p.caution_soft);
            } else {
                status_pill(ui, "● REC", p.danger, p.danger_soft);
            }
        });

        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(fmt_duration(elapsed))
                    .font(FontId::monospace(40.0))
                    .family(crate::theme::bold())
                    .color(if paused { p.muted } else { p.text }),
            );
            ui.label(
                RichText::new(if paused {
                    "Recording paused"
                } else {
                    "Recording in progress"
                })
                .size(12.0)
                .color(p.muted),
            );
        });
        ui.add_space(12.0);

        mask(ui, |ui| {
            ui.columns(if self.max_duration_secs > 0 { 3 } else { 2 }, |columns| {
                stat(
                    &mut columns[0],
                    "FRAME RATE",
                    &format!("{:.0} fps", self.display_fps),
                );
                stat(&mut columns[1], "FILE SIZE", &size);
                if self.max_duration_secs > 0 {
                    let left = self.max_duration_secs.saturating_sub(elapsed.as_secs());
                    stat(
                        &mut columns[2],
                        "STOPS IN",
                        &fmt_duration(Duration::from_secs(left)),
                    );
                }
            });

            if let (Some(level), true) = (level, audio_on) {
                ui.add_space(6.0);
                let level = (level as f32 / 100.0).clamp(0.0, 1.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Audio").size(12.0).color(p.muted));
                    let meter_w = (ui.available_width() - 44.0).max(60.0);
                    level_meter(ui, level, meter_w);
                    ui.label(
                        RichText::new(format!("{:.0}%", level * 100.0))
                            .size(11.5)
                            .color(p.muted_dim),
                    );
                });
            } else if !audio_on {
                ui.add_space(4.0);
                ui.label(RichText::new("Audio is off").size(12.0).color(p.muted_dim));
            }
        });

        if let Some(message) = warning {
            ui.add_space(8.0);
            info_note(ui, Icon::Alert, &message, p.caution);
        }

        ui.add_space(12.0);
        let mut pause_clicked = false;
        let mut stop_clicked = false;
        let mut screenshot_clicked = false;

        ui.horizontal(|ui| {
            let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            let pause_kind = if paused {
                ButtonKind::Success
            } else {
                ButtonKind::Secondary
            };
            if button_sized(
                ui,
                Some(if paused { Icon::Play } else { Icon::Pause }),
                if paused { "Resume" } else { "Pause" },
                pause_kind,
                half,
                40.0,
            )
            .on_hover_text_if(shortcuts, "F10")
            .clicked()
            {
                pause_clicked = true;
            }
            if button_sized(
                ui,
                Some(Icon::Stop),
                "Stop & save",
                ButtonKind::Danger,
                half,
                40.0,
            )
            .on_hover_text_if(shortcuts, "F9")
            .clicked()
            {
                stop_clicked = true;
            }
        });

        ui.add_space(8.0);
        if wide_button(
            ui,
            Some(Icon::Screenshot),
            "Take screenshot",
            ButtonKind::Secondary,
            36.0,
        )
        .on_hover_text_if(shortcuts, "F11")
        .clicked()
        {
            screenshot_clicked = true;
        }

        if pause_clicked {
            self.toggle_pause();
        }
        if stop_clicked {
            self.begin_stop();
        } else if screenshot_clicked {
            self.take_screenshot();
        }
    }

    fn destination_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_header(ui, Icon::Folder, "Save to", "Where captures are stored");
            ui.add_space(10.0);

            let mut browse = None;
            let mut open = None;
            for (kind, label) in [
                (FolderKind::Recordings, "Recordings"),
                (FolderKind::Screenshots, "Screenshots"),
            ] {
                if kind == FolderKind::Screenshots {
                    ui.add_space(4.0);
                }
                field_label(
                    ui,
                    Some(if kind == FolderKind::Recordings {
                        Icon::Film
                    } else {
                        Icon::Image
                    }),
                    label,
                );
                let value = match kind {
                    FolderKind::Recordings => &mut self.output_dir,
                    FolderKind::Screenshots => &mut self.screenshot_dir,
                };
                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let input_w = (ui.available_width() - 32.0 * 2.0 - gap * 2.0).max(80.0);
                    let p = pal();
                    let input = egui::TextEdit::singleline(value)
                        .text_color(p.text)
                        .hint_text("Choose a folder")
                        .frame(
                            egui::Frame::new()
                                .fill(p.input_bg)
                                .corner_radius(CornerRadius::same(4))
                                .stroke(Stroke::new(1.0, p.border))
                                .inner_margin(Margin::symmetric(10, 7)),
                        );
                    ui.add_sized(vec2(input_w, 32.0), input)
                        .on_hover_text(value.as_str());
                    if icon_button_sized(ui, Icon::Folder, "Browse…", 32.0).clicked() {
                        browse = Some(kind);
                    }
                    if icon_button_sized(ui, Icon::Open, "Open folder", 32.0).clicked() {
                        open = Some(kind);
                    }
                });
            }

            if let Some(kind) = browse {
                self.browse_folder(kind);
            }
            match open {
                Some(FolderKind::Recordings) => {
                    let dir = self.output_dir.clone();
                    self.open_folder(&dir, util::recordings_dir);
                }
                Some(FolderKind::Screenshots) => {
                    let dir = self.screenshot_dir.clone();
                    self.open_folder(&dir, util::screenshots_dir);
                }
                None => {}
            }
        });
    }

    fn recent_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            let p = pal();
            let mut open_recordings = false;
            ui.horizontal(|ui| {
                section_header_left(ui, Icon::Film, "Recent", "Latest captures");
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Open, "Open recordings folder").clicked() {
                        open_recordings = true;
                    }
                });
            });
            if open_recordings {
                let dir = self.output_dir.clone();
                self.open_folder(&dir, util::recordings_dir);
            }
            ui.add_space(10.0);

            if self.history.is_empty() {
                empty_state(
                    ui,
                    Icon::Film,
                    "No captures yet",
                    "Recordings and screenshots will appear here.",
                );
                return;
            }

            let limit = if self.show_all_history {
                self.history.len()
            } else {
                HISTORY_PREVIEW
            };
            let entries: Vec<HistoryEntry> = self.history.iter().take(limit).cloned().collect();

            let mut request_delete: Option<String> = None;
            let mut confirm_delete: Option<String> = None;
            let mut cancel_delete = false;
            let mut open: Option<(PathBuf, bool)> = None;
            let mut edit: Option<PathBuf> = None;

            for (row_index, entry) in entries.iter().enumerate() {
                if row_index > 0 {
                    ui.add_space(6.0);
                }
                let path = entry.path_buf();
                let is_pending = self.pending_delete.as_deref() == Some(entry.path.as_str());
                let screenshot = entry.kind == "screenshot";
                let (fg, bg) = if screenshot {
                    (p.success_text, p.success_soft)
                } else {
                    (p.accent_hi, p.accent_soft)
                };

                let row = egui::Frame::new()
                    .fill(if is_pending {
                        p.danger_soft
                    } else {
                        p.surface_elevated
                    })
                    .corner_radius(CornerRadius::same(4))
                    .stroke(Stroke::new(
                        1.0,
                        if is_pending {
                            p.danger_border
                        } else {
                            p.border_soft
                        },
                    ))
                    .inner_margin(Margin::symmetric(8, 7))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if is_pending {
                            ui.horizontal(|ui| {
                                icon_badge_sized(ui, Icon::Trash, p.danger, p.surface, 30.0);
                                ui.add_space(6.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new("Delete this file?")
                                            .size(12.5)
                                            .family(crate::theme::bold())
                                            .color(p.text),
                                    );
                                    ui.label(
                                        RichText::new("It is removed from disk permanently.")
                                            .size(11.5)
                                            .color(p.danger_text),
                                    );
                                });
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                let half =
                                    (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                                if button_sized(
                                    ui,
                                    None,
                                    "Cancel",
                                    ButtonKind::Secondary,
                                    half,
                                    30.0,
                                )
                                .clicked()
                                {
                                    cancel_delete = true;
                                }
                                if button_sized(
                                    ui,
                                    Some(Icon::Trash),
                                    "Delete",
                                    ButtonKind::Danger,
                                    half,
                                    30.0,
                                )
                                .clicked()
                                {
                                    confirm_delete = Some(entry.path.clone());
                                }
                            });
                            return;
                        }

                        ui.horizontal(|ui| {
                            icon_badge_sized(
                                ui,
                                if screenshot { Icon::Image } else { Icon::Film },
                                fg,
                                bg,
                                30.0,
                            );
                            ui.add_space(4.0);

                            let actions = if screenshot { 3.0 } else { 4.0 };
                            let actions_w = actions * 28.0 + (actions - 1.0) * 2.0;
                            let text_w = (ui.available_width() - actions_w - 8.0).max(60.0);
                            ui.allocate_ui_with_layout(
                                vec2(text_w, 32.0),
                                egui::Layout::top_down(Align::Min),
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    let title = if screenshot {
                                        "Screenshot".to_string()
                                    } else {
                                        format!(
                                            "Recording · {}",
                                            util::human_duration(entry.duration_secs.round() as u64)
                                        )
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(title).size(12.5).color(p.text),
                                        )
                                        .truncate(),
                                    );
                                    let detail = format!(
                                        "{} · {}",
                                        util::time_ago(entry.created),
                                        util::human_size(entry.size)
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(detail).size(11.5).color(p.muted_dim),
                                        )
                                        .truncate(),
                                    );
                                },
                            );

                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if icon_button_sized(ui, Icon::Trash, "Delete", 28.0).clicked() {
                                    request_delete = Some(entry.path.clone());
                                }
                                if icon_button_sized(ui, Icon::Folder, "Show in folder", 28.0)
                                    .clicked()
                                {
                                    open = Some((path.clone(), true));
                                }
                                if !screenshot
                                    && icon_button_sized(ui, Icon::Edit, "Edit in editor", 28.0)
                                        .clicked()
                                {
                                    edit = Some(path.clone());
                                }
                                let (icon, tip) = if screenshot {
                                    (Icon::Open, "Open")
                                } else {
                                    (Icon::Play, "Play")
                                };
                                if icon_button_sized(ui, icon, tip, 28.0).clicked() {
                                    open = Some((path.clone(), false));
                                }
                            });
                        });
                    });
                if !is_pending {
                    row.response.on_hover_text(entry.file_name());
                }
            }

            if self.history.len() > HISTORY_PREVIEW {
                ui.add_space(6.0);
                let text = if self.show_all_history {
                    "Show fewer".to_string()
                } else {
                    format!("Show all {}", self.history.len())
                };
                ui.vertical_centered(|ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new(text).size(12.0).color(p.accent_hi))
                                .frame(false),
                        )
                        .clicked()
                    {
                        self.show_all_history = !self.show_all_history;
                    }
                });
            }

            if let Some(path) = request_delete {
                self.pending_delete = Some(path);
            }
            if cancel_delete {
                self.pending_delete = None;
            }
            if let Some(path) = confirm_delete {
                self.pending_delete = None;
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        self.error = Some(format!(
                            "Could not delete {}: {err}",
                            file_name(Path::new(&path))
                        ));
                        return;
                    }
                }
                if self.saved.as_deref() == Some(Path::new(&path)) {
                    self.saved = None;
                }
                self.history.retain(|entry| entry.path != path);
                history::save(&self.history);
            }
            if let Some((path, reveal)) = open {
                self.open_capture(&path, reveal);
            }
            if let Some(path) = edit {
                self.edit_capture(&path, ui.ctx());
            }
        });
    }

    fn notifications(&mut self, ui: &mut egui::Ui) {
        if self.saved.is_some() {
            self.saved_toast(ui);
            ui.add_space(12.0);
        }
        if self.error.is_some() {
            self.error_banner(ui);
            ui.add_space(12.0);
        }
    }

    fn saved_toast(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.saved.clone() else {
            return;
        };
        let p = pal();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let is_video = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));

        let mut dismiss = false;
        let mut edit = false;
        let mut action: Option<bool> = None;
        egui::Frame::new()
            .fill(p.success_soft)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, p.success.gamma_multiply(0.4)))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    icon_badge_sized(ui, Icon::Check, p.success_text, p.surface, 30.0);
                    ui.add_space(4.0);
                    let text_w = (ui.available_width() - 36.0).max(80.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_w, 32.0),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(
                                RichText::new(if is_video {
                                    "Recording saved"
                                } else {
                                    "Screenshot saved"
                                })
                                .size(13.0)
                                .family(crate::theme::bold())
                                .color(p.text),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} · {}",
                                        file_name(&path),
                                        util::human_size(size)
                                    ))
                                    .size(11.5)
                                    .color(p.muted),
                                )
                                .truncate(),
                            );
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, "Dismiss").clicked() {
                            dismiss = true;
                        }
                    });
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let count = if is_video { 3.0 } else { 2.0 };
                    let w = (ui.available_width() - (count - 1.0) * ui.spacing().item_spacing.x)
                        / count;
                    if is_video
                        && button_sized(ui, Some(Icon::Edit), "Edit", ButtonKind::Primary, w, 32.0)
                            .clicked()
                    {
                        edit = true;
                    }
                    if button_sized(
                        ui,
                        Some(if is_video { Icon::Play } else { Icon::Open }),
                        if is_video { "Play" } else { "Open" },
                        ButtonKind::Secondary,
                        w,
                        32.0,
                    )
                    .clicked()
                    {
                        action = Some(false);
                    }
                    if button_sized(
                        ui,
                        Some(Icon::Folder),
                        "Show",
                        ButtonKind::Secondary,
                        w,
                        32.0,
                    )
                    .on_hover_text("Show in folder")
                    .clicked()
                    {
                        action = Some(true);
                    }
                });
            });

        if dismiss {
            self.saved = None;
        }
        if let Some(reveal) = action {
            self.open_capture(&path, reveal);
        }
        if edit {
            self.edit_capture(&path, ui.ctx());
        }
    }

    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(message) = self.error.clone() else {
            return;
        };
        let p = pal();

        egui::Frame::new()
            .fill(p.danger_soft)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, p.danger_border))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    icon_badge_sized(ui, Icon::Alert, p.danger, p.surface, 30.0);
                    ui.add_space(4.0);

                    let text_w = (ui.available_width() - 38.0).max(100.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_w, 0.0),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            ui.label(
                                RichText::new("Something went wrong")
                                    .size(13.0)
                                    .family(crate::theme::bold())
                                    .color(p.text),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&message).size(12.0).color(p.danger_text),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        },
                    );

                    ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                        if icon_button(ui, Icon::Close, "Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                });
            });
    }

    fn quit_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_quit {
            return;
        }
        let p = pal();
        let recording_active =
            self.recording.is_some() || matches!(self.task, Task::Starting(_) | Task::Stopping(_));
        if !recording_active && !self.quit_after_stop {
            self.quit_edits_dialog(ctx);
            return;
        }
        let stopping = matches!(self.task, Task::Stopping(_)) || self.quit_after_stop;
        let modal = egui::Modal::new(egui::Id::NULL.with("confirm-quit"))
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.horizontal(|ui| {
                    icon_badge_sized(ui, Icon::Record, p.danger, p.danger_soft, 34.0);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(if stopping {
                                "Saving your recording…"
                            } else {
                                "A recording is in progress"
                            })
                            .size(14.5)
                            .family(crate::theme::bold())
                            .color(p.text),
                        );
                        ui.label(
                            RichText::new(if stopping {
                                "Scroder closes as soon as the file is written."
                            } else {
                                "Stop and save it before quitting?"
                            })
                            .size(12.0)
                            .color(p.muted),
                        );
                    });
                });
                ui.add_space(16.0);
                if stopping {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new().size(22.0).color(p.accent_hi));
                    });
                    return;
                }
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                    if button_sized(
                        ui,
                        None,
                        "Keep recording",
                        ButtonKind::Secondary,
                        half,
                        36.0,
                    )
                    .clicked()
                    {
                        self.confirm_quit = false;
                    }
                    if button_sized(
                        ui,
                        Some(Icon::Stop),
                        "Stop & quit",
                        ButtonKind::Danger,
                        half,
                        36.0,
                    )
                    .clicked()
                    {
                        self.quit_after_stop = true;
                        if self.recording.is_some() {
                            self.begin_stop();
                        }
                    }
                });
            });
        if modal.should_close() && !stopping {
            self.confirm_quit = false;
        }
        // A start that was still in flight: stop it as soon as it is running.
        if self.quit_after_stop && self.recording.is_some() {
            self.begin_stop();
        }
        if self.quit_after_stop && matches!(self.task, Task::Idle) && self.recording.is_none() {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl App {
    /// A compact always-on-top control bar while recording (Windows keeps it
    /// out of the capture itself).
    fn recording_bar(&mut self, ctx: &egui::Context) {
        let show = cfg!(windows) && self.minimize_on_record && self.recording.is_some();
        if !show {
            self.bar_excluded = false;
            self.minimize_pending = None;
            return;
        }
        let Some(recording) = &self.recording else {
            return;
        };
        let elapsed = recording.elapsed();
        let paused = recording.is_paused();
        let primary = self
            .monitors
            .iter()
            .find(|m| m.primary)
            .or_else(|| self.monitors.first());
        let size = vec2(372.0, 56.0);
        let position = primary
            .map(|m| {
                let (x, y) = m.logical_position();
                let (w, _) = m.logical_size();
                pos2(x + (w - size.x) / 2.0, y + 16.0)
            })
            .unwrap_or(pos2(200.0, 16.0));
        let builder = egui::ViewportBuilder::default()
            .with_title(RECORDING_BAR_TITLE)
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_icon(window_icon())
            .with_position(position)
            .with_inner_size(size);

        let mut action = None;
        let bar_id = egui::ViewportId::from_hash_of("scroder-recording-bar");
        ctx.show_viewport_immediate(bar_id, builder, |ui, _class| {
            let p = pal();
            let ctx = ui.ctx().clone();
            let rect = ctx.content_rect();
            ui.painter().rect(
                rect,
                CornerRadius::same(theme::RADIUS_PANEL),
                p.panel_top,
                Stroke::new(1.0, p.border_hi),
                StrokeKind::Inside,
            );
            let drag = ui.interact(rect, ui.scope_id().with("bar-drag"), Sense::drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 10.0))),
                |ui| {
                    ui.horizontal_centered(|ui| {
                        let pulse = (ctx.input(|i| i.time) * 2.0).sin() * 0.5 + 0.5;
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        let color = if paused {
                            p.caution
                        } else {
                            p.danger.gamma_multiply(0.55 + 0.45 * pulse as f32)
                        };
                        ui.painter().circle_filled(dot.center(), 6.0, color);
                        ui.label(
                            RichText::new(fmt_duration(elapsed))
                                .font(FontId::monospace(17.0))
                                .color(p.text),
                        );
                        ui.label(
                            RichText::new(if paused { "Paused" } else { "Recording" })
                                .size(12.5)
                                .color(p.muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if icon_button_sized(ui, Icon::Open, "Show Scroder", 34.0).clicked() {
                                action = Some(2);
                            }
                            if button_sized(
                                ui,
                                Some(Icon::Stop),
                                "Stop",
                                ButtonKind::Danger,
                                82.0,
                                34.0,
                            )
                            .on_hover_text("Stop and save (F9)")
                            .clicked()
                            {
                                action = Some(1);
                            }
                            let (icon, tip) = if paused {
                                (Icon::Play, "Resume (F10)")
                            } else {
                                (Icon::Pause, "Pause (F10)")
                            };
                            if icon_button_sized(ui, icon, tip, 34.0).clicked() {
                                action = Some(0);
                            }
                        });
                    });
                },
            );
            ctx.request_repaint_after(Duration::from_millis(250));
        });

        if !self.bar_excluded {
            self.bar_excluded = backend::exclude_from_capture(RECORDING_BAR_TITLE);
        }
        if let Some(since) = self.minimize_pending {
            let bar_visible = ctx.input(|i| {
                i.raw
                    .viewports
                    .get(&bar_id)
                    .and_then(|v| v.visible())
                    .unwrap_or(false)
            });
            if (bar_visible && self.bar_excluded) || since.elapsed() > Duration::from_millis(1500) {
                self.minimize_pending = None;
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::ROOT,
                    egui::ViewportCommand::Minimized(true),
                );
            } else {
                ctx.request_repaint_after(Duration::from_millis(30));
            }
        }
        match action {
            Some(0) => self.toggle_pause(),
            Some(1) => self.begin_stop(),
            Some(2) => {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::ROOT,
                    egui::ViewportCommand::Minimized(false),
                );
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            }
            _ => {}
        }
    }

    /// Camtasia-style prompt: Save / Don't Save / Cancel.
    fn quit_edits_dialog(&mut self, ctx: &egui::Context) {
        let p = pal();
        let exporting = self.editor.is_exporting();
        let name = self.editor.project_name();
        let mut choice: Option<u8> = None;
        let modal = egui::Modal::new(egui::Id::NULL.with("confirm-quit-edits"))
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(theme::RADIUS_PANEL))
                    .inner_margin(Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(
                    RichText::new(crate::APP_NAME)
                        .size(15.0)
                        .family(theme::bold())
                        .color(p.text),
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if exporting {
                        "An export is still running. Quitting now cancels it.".to_string()
                    } else {
                        format!("Save changes to the project \"{name}\" before closing?")
                    })
                    .size(13.0)
                    .color(p.text_soft),
                );
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if exporting {
                        if button_sized(
                            ui,
                            None,
                            "Keep exporting",
                            ButtonKind::Primary,
                            130.0,
                            32.0,
                        )
                        .clicked()
                        {
                            choice = Some(2);
                        }
                        if button_sized(ui, None, "Quit", ButtonKind::Secondary, 80.0, 32.0)
                            .clicked()
                        {
                            choice = Some(1);
                        }
                        return;
                    }
                    if button_sized(ui, None, "Cancel", ButtonKind::Secondary, 84.0, 32.0).clicked()
                    {
                        choice = Some(2);
                    }
                    if button_sized(ui, None, "Don't Save", ButtonKind::Secondary, 100.0, 32.0)
                        .clicked()
                    {
                        choice = Some(1);
                    }
                    if button_sized(ui, None, "Save", ButtonKind::Primary, 84.0, 32.0).clicked() {
                        choice = Some(0);
                    }
                });
            });
        if modal.should_close() && choice.is_none() {
            choice = Some(2);
        }
        match choice {
            Some(0) => {
                if self.editor.save_project(false) {
                    self.allow_close = true;
                    self.confirm_quit = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(1) => {
                self.allow_close = true;
                self.confirm_quit = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(_) => self.confirm_quit = false,
            None => {}
        }
    }

    fn handle_editor_events(&mut self, events: Vec<EditorEvent>) {
        for event in events {
            match event {
                EditorEvent::Exported(path, duration) => {
                    self.history = history::add(
                        self.history.clone(),
                        HistoryEntry::new(&path, "video", duration),
                    );
                }
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        });
        let mut imported = false;
        for path in dropped {
            if editor::is_video(&path) || editor::is_project(&path) {
                self.editor.open(path, true, ctx);
                imported = true;
            } else {
                self.error = Some(format!(
                    "{} is not a supported video file.",
                    file_name(&path)
                ));
            }
        }
        if imported {
            self.page = Page::Edit;
        }
    }
}

impl eframe::App for App {
    /// Runs every pass, even while the window is minimized (hotkeys, auto-stop,
    /// finishing recordings, ...).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.titlebar_themed {
            // The native window exists now; make its title bar dark too.
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            let c = pal().panel_top;
            backend::style_title_bar(&self.window_title, [c.r(), c.g(), c.b()]);
            self.titlebar_themed = true;
        }
        self.handle_close(ctx);
        self.poll_tasks(ctx);
        self.update_stats();
        self.update_window_title(ctx);
        self.autosave_prefs();

        if self.recording.is_some()
            || !matches!(self.task, Task::Idle)
            || self.region_picker.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drive_region_picker(&ctx);
        self.recording_bar(&ctx);

        let p = pal();

        egui::Panel::top("app_header")
            .exact_size(60.0)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(ui, |ui| {
                self.header(ui);
            });

        egui::Panel::bottom("app_status")
            .exact_size(30.0)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(ui, |ui| {
                self.status_bar(ui);
            });

        self.handle_dropped_files(&ctx);

        if self.page == Page::Edit {
            let events = self.editor.ui(ui);
            self.handle_editor_events(events);
            self.quit_dialog(&ctx);
            return;
        }

        self.drive_preview(&ctx);

        // Layout: GUTTER | settings | GUTTER | controls | GUTTER. Each column's
        // right gutter also hosts its (floating) scrollbar.
        let side_w = if ctx.content_rect().width() < 1100.0 {
            340.0
        } else {
            372.0
        };
        egui::Panel::right("control_panel")
            .exact_size(side_w)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 0,
                right: 0,
                top: GUTTER as i8,
                bottom: GUTTER as i8,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("control_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() - GUTTER);
                        self.setup_card(ui);
                        self.notifications(ui);
                        self.controls(ui);
                        ui.add_space(12.0);
                        self.recent_card(ui);
                        ui.add_space(12.0);
                        let locked = self.is_busy();
                        ui.add_enabled_ui(!locked, |ui| self.destination_card(ui));
                    });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: GUTTER as i8,
                right: 0,
                top: GUTTER as i8,
                bottom: GUTTER as i8,
            }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() - GUTTER);
                        let locked = self.is_busy();
                        if locked {
                            info_note(
                                ui,
                                Icon::Info,
                                "Settings are locked while recording. Stop the recording to change them.",
                                p.text_soft,
                            );
                            ui.add_space(8.0);
                        }
                        ui.add_enabled_ui(!locked, |ui| {
                            self.recorder_strip(ui);
                            ui.add_space(12.0);
                            self.source_card(ui);
                            ui.add_space(12.0);
                            self.video_card(ui);
                            if self.webcam_enabled {
                                ui.add_space(12.0);
                                self.webcam_card(ui);
                            }
                            ui.add_space(12.0);
                            self.preferences_card(ui);
                        });
                    });
            });

        self.quit_dialog(&ctx);
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    Primary,
    Secondary,
    Danger,
    Success,
}

pub(crate) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = pal();
    egui::Frame::new()
        .fill(p.surface)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(14))
        .stroke(Stroke::new(1.0, p.border))
        .shadow(p.card_shadow)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub(crate) fn mask<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = pal();
    egui::Frame::new()
        .fill(p.input_bg)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::same(10))
        .stroke(Stroke::new(1.0, p.border_soft))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub(crate) fn info_note(ui: &mut egui::Ui, icon: Icon, text: &str, color: Color32) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
        icons::paint(ui.painter(), rect.translate(vec2(0.0, 1.0)), icon, color);
        ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
    });
}

pub(crate) fn command_box(ui: &mut egui::Ui, command: &str) {
    let p = pal();
    egui::Frame::new()
        .fill(p.surface)
        .corner_radius(CornerRadius::same(4))
        .stroke(Stroke::new(1.0, p.border))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let text_w = (ui.available_width() - 34.0).max(60.0);
                ui.allocate_ui_with_layout(
                    vec2(text_w, 22.0),
                    egui::Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(command)
                                    .font(FontId::monospace(11.0))
                                    .color(p.text),
                            )
                            .selectable(true)
                            .wrap(),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if icon_button_sized(ui, Icon::Copy, "Copy command", 26.0).clicked() {
                        ui.ctx().copy_text(command.to_string());
                    }
                });
            });
        });
}

pub(crate) fn section_header_left(ui: &mut egui::Ui, icon: Icon, title: &str, subtitle: &str) {
    let p = pal();
    icon_badge(ui, icon, p.accent_hi, p.accent_soft);
    ui.add_space(4.0);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(
            RichText::new(title)
                .size(14.0)
                .family(crate::theme::bold())
                .color(p.text),
        );
        ui.label(RichText::new(subtitle).size(12.0).color(p.muted));
    });
}

pub(crate) fn section_header(ui: &mut egui::Ui, icon: Icon, title: &str, subtitle: &str) {
    ui.horizontal(|ui| section_header_left(ui, icon, title, subtitle));
}

fn icon_badge(ui: &mut egui::Ui, icon: Icon, fg: Color32, bg: Color32) {
    icon_badge_sized(ui, icon, fg, bg, 30.0);
}

pub(crate) fn icon_badge_sized(ui: &mut egui::Ui, icon: Icon, fg: Color32, bg: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let radius = CornerRadius::same((size * 0.28).round().clamp(0.0, 255.0) as u8);
    ui.painter().rect_filled(rect, radius, bg);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(size * 0.52)),
        icon,
        fg,
    );
}

fn focus_ring(ui: &egui::Ui, response: &egui::Response, rect: Rect, radius: u8) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(radius.saturating_add(2)),
            Stroke::new(2.0, pal().focus),
            StrokeKind::Outside,
        );
    }
}

pub(crate) fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    icon_button_sized(ui, icon, tooltip, 30.0)
}

pub(crate) fn icon_button_sized(
    ui: &mut egui::Ui,
    icon: Icon,
    tooltip: &str,
    size: f32,
) -> egui::Response {
    let p = pal();
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tooltip));
    let hovered = response.hovered() && ui.is_enabled();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), p.surface_hover);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let color = if !ui.is_enabled() {
        p.muted_dim
    } else if hovered {
        p.text
    } else {
        p.text_soft
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(size * 0.58)),
        icon,
        color,
    );
    focus_ring(ui, &response, rect, 7);
    response.on_hover_text(tooltip)
}

pub(crate) fn keycap(ui: &mut egui::Ui, key: &str) {
    let p = pal();
    let galley = ui
        .painter()
        .layout_no_wrap(key.to_string(), FontId::monospace(10.0), p.text_soft);
    let width = galley.size().x + 12.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(4),
        p.surface_elevated,
        Stroke::new(1.0, p.border_hi),
        StrokeKind::Middle,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, p.text_soft);
}

fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32, border: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(11.5), fg);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 14.0, 22.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(theme::RADIUS),
        bg,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
}

pub(crate) fn status_pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(11.5), fg);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 18.0, 20.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(theme::RADIUS),
        bg,
        Stroke::new(1.0, fg.gamma_multiply(0.35)),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 5.0, color.gamma_multiply(0.18));
    ui.painter().circle_filled(rect.center(), 2.8, color);
}

pub(crate) fn field_label(ui: &mut egui::Ui, icon: Option<Icon>, text: &str) {
    let p = pal();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        if let Some(icon) = icon {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            icons::paint(ui.painter(), rect, icon, p.muted);
        }
        ui.label(RichText::new(text).size(12.0).color(p.muted));
    });
}

/// Dropdown with a refresh button next to it.
fn picker_row(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    labels: &[String],
    selected: &mut usize,
    refresh: &mut bool,
) {
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let width = (ui.available_width() - 32.0 - gap).max(100.0);
        ui.allocate_ui_with_layout(
            vec2(width, 32.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                select_index(ui, id, current, labels, selected);
            },
        );
        if icon_button_sized(ui, Icon::Refresh, "Refresh list", 32.0).clicked() {
            *refresh = true;
        }
    });
}

pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    options: &[(T, Option<Icon>, &str)],
) -> egui::Response {
    let size = vec2(ui.available_width(), 36.0);
    segmented_sized(ui, id, value, options, size)
}

pub(crate) fn segmented_sized<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    options: &[(T, Option<Icon>, &str)],
    size: Vec2,
) -> egui::Response {
    let p = pal();
    let (rect, mut outer) = ui.allocate_exact_size(size, Sense::hover());
    if options.is_empty() {
        return outer;
    }
    ui.painter().rect(
        rect,
        CornerRadius::same(4),
        p.input_bg,
        Stroke::new(1.0, p.border_soft),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(3.0);
    let seg_w = inner.width() / options.len() as f32;
    let enabled = ui.is_enabled();
    let font = FontId::proportional(if size.y < 34.0 { 12.0 } else { 12.5 });

    for (i, (option, icon, label)) in options.iter().enumerate() {
        let seg = Rect::from_min_size(
            pos2(inner.left() + seg_w * i as f32, inner.top()),
            vec2(seg_w, inner.height()),
        );
        let response = ui.interact(seg, ui.scope_id().with((id, i)), Sense::click());
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, *label)
        });

        if response.clicked() && !selected {
            *value = *option;
            outer.mark_changed();
        }

        let selected = *value == *option;
        let hovered = response.hovered() && enabled;
        if selected {
            ui.painter().rect(
                seg.shrink(1.0),
                CornerRadius::same(3),
                p.surface_active,
                Stroke::new(1.0, p.border_hi),
                StrokeKind::Inside,
            );
        } else if hovered {
            ui.painter()
                .rect_filled(seg.shrink(1.0), CornerRadius::same(3), p.surface_hover);
        }
        focus_ring(ui, &response, seg.shrink(1.0), 3);

        let fg = if selected {
            p.text
        } else if hovered {
            p.text_soft
        } else {
            p.muted
        };
        let galley = ui
            .painter()
            .layout_no_wrap((*label).to_string(), font.clone(), fg);
        let icon_w = if icon.is_some() { 15.0 + 6.0 } else { 0.0 };
        let total = icon_w + galley.size().x;
        let start_x = seg.center().x - total / 2.0;

        if let Some(icon) = icon {
            icons::paint(
                ui.painter(),
                Rect::from_center_size(pos2(start_x + 7.5, seg.center().y), Vec2::splat(15.0)),
                *icon,
                if selected { p.accent_hi } else { p.muted_dim },
            );
        }
        ui.painter().galley(
            pos2(start_x + icon_w, seg.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );

        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }

    outer
}

pub(crate) fn toggle_row(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    subtitle: &str,
    value: &mut bool,
) -> bool {
    let p = pal();
    let height = if subtitle.is_empty() { 44.0 } else { 52.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let enabled = ui.is_enabled();
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, *value, title));

    let clicked = response.clicked();
    if clicked {
        *value = !*value;
    }

    let hovered = response.hovered() && enabled;
    // Content is flush with the card's other rows (icon at the left edge, switch
    // at the right edge); only the hover highlight bleeds slightly outside.
    let highlight = rect.expand2(vec2(6.0, 0.0));
    if hovered {
        ui.painter().rect_filled(
            highlight,
            CornerRadius::same(theme::RADIUS),
            p.surface_hover,
        );
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    focus_ring(ui, &response, highlight, theme::RADIUS);

    let painter = ui.painter();
    icons::paint(
        painter,
        Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), Vec2::splat(18.0)),
        icon,
        if *value { p.accent_hi } else { p.text_soft },
    );

    let text_x = rect.left() + 30.0;
    let max_text = (rect.width() - 30.0 - 56.0).max(40.0);
    let title_galley = painter.layout(
        title.to_string(),
        FontId::proportional(13.5),
        p.text,
        max_text,
    );
    if subtitle.is_empty() {
        painter.galley(
            pos2(text_x, rect.center().y - title_galley.size().y / 2.0),
            title_galley,
            p.text,
        );
    } else {
        let sub_galley =
            painter.layout_no_wrap(subtitle.to_string(), FontId::proportional(12.0), p.muted);
        let sub_galley = if sub_galley.size().x > max_text {
            let mut job = egui::text::LayoutJob::simple_singleline(
                subtitle.to_string(),
                FontId::proportional(12.0),
                p.muted,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(max_text);
            painter.layout_job(job)
        } else {
            sub_galley
        };
        painter.galley(
            pos2(text_x, rect.center().y - title_galley.size().y - 1.0),
            title_galley,
            p.text,
        );
        painter.galley(pos2(text_x, rect.center().y + 2.0), sub_galley, p.muted);
    }

    let switch =
        Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(40.0, 22.0));
    paint_toggle(ui.ctx(), ui.painter(), response.id, switch, *value, hovered);

    if !subtitle.is_empty() {
        response.on_hover_text(subtitle);
    }
    clicked
}

fn paint_toggle(
    ctx: &egui::Context,
    painter: &egui::Painter,
    id: egui::Id,
    rect: Rect,
    on: bool,
    hovered: bool,
) {
    let p = pal();
    let t = ctx.animate_bool_with_time(id, on, 0.14);
    let track = if on {
        if hovered {
            p.accent_hi
        } else {
            p.accent
        }
    } else if hovered {
        p.toggle_off_hover
    } else {
        p.toggle_off
    };

    painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), track);

    let padding = 3.0;
    let radius = rect.height() / 2.0 - padding;
    let left = rect.left() + padding + radius;
    let right = rect.right() - padding - radius;
    let knob_x = left + (right - left) * t;

    painter.circle_filled(
        pos2(knob_x, rect.center().y + 1.0),
        radius,
        Color32::from_black_alpha(40),
    );
    painter.circle_filled(
        pos2(knob_x, rect.center().y),
        radius,
        if on { Color32::WHITE } else { p.knob_off },
    );
}

fn select_index(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    labels: &[String],
    selected: &mut usize,
) -> bool {
    let p = pal();
    let before = *selected;
    combo_metrics(ui);
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(current).size(12.5).color(p.text))
        .width(ui.available_width())
        .height(320.0)
        .truncate()
        .show_ui(ui, |ui| {
            if labels.is_empty() {
                ui.label(RichText::new("Nothing found").size(12.0).color(p.muted));
            }
            for (index, label) in labels.iter().enumerate() {
                ui.selectable_value(selected, index, label.as_str());
            }
        });
    *selected != before
}

pub(crate) fn select_enum<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    options: &[(T, &str)],
    value: &mut T,
) -> bool {
    let p = pal();
    let before = *value;
    combo_metrics(ui);
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(current).size(12.5).color(p.text))
        .width(ui.available_width())
        .truncate()
        .show_ui(ui, |ui| {
            for (option, label) in options {
                ui.selectable_value(value, *option, *label);
            }
        });
    *value != before
}

/// Makes dropdowns exactly as tall as the buttons and inputs beside them.
fn combo_metrics(ui: &mut egui::Ui) {
    let text_h = ui.text_style_height(&egui::TextStyle::Button);
    let spacing = ui.spacing_mut();
    spacing.interact_size.y = CONTROL_H;
    spacing.button_padding.y = ((CONTROL_H - text_h) / 2.0).max(4.0);
}

fn number_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
) {
    let p = pal();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(RichText::new(label).size(11.5).color(p.muted_dim));
        ui.add_sized(
            vec2(76.0, CONTROL_H),
            egui::DragValue::new(value)
                .range(range)
                .speed(1.0)
                .suffix(" px"),
        );
    });
}

pub(crate) fn button_sized(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    kind: ButtonKind,
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let hovered = response.hovered() && enabled;
    paint_button(ui.painter(), rect, icon, label, kind, enabled, hovered);
    focus_ring(ui, &response, rect, 8);
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

pub(crate) fn wide_button(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    kind: ButtonKind,
    height: f32,
) -> egui::Response {
    button_sized(ui, icon, label, kind, ui.available_width(), height)
}

fn paint_button(
    painter: &egui::Painter,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    kind: ButtonKind,
    enabled: bool,
    hovered: bool,
) {
    let p = pal();
    let (fill, stroke_color, fg) = match (kind, enabled, hovered) {
        (_, false, _) => (p.surface_elevated, p.border_soft, p.muted_dim),
        (ButtonKind::Primary, true, false) => (p.accent, p.accent, Color32::WHITE),
        (ButtonKind::Primary, true, true) => (p.accent_hi, p.accent_hi, Color32::WHITE),
        (ButtonKind::Danger, true, false) => (p.surface_elevated, p.border, p.danger_text),
        (ButtonKind::Danger, true, true) => (p.danger_soft, p.danger_border, p.danger_text),
        (ButtonKind::Success, true, false) => (p.success, p.success, p.on_success),
        (ButtonKind::Success, true, true) => (p.success_hi, p.success_hi, p.on_success),
        (ButtonKind::Secondary, true, false) => (p.surface_elevated, p.border, p.text),
        (ButtonKind::Secondary, true, true) => (p.surface_hover, p.border_hi, p.text),
    };

    painter.rect(
        rect,
        CornerRadius::same(4),
        fill,
        Stroke::new(1.0, stroke_color),
        StrokeKind::Inside,
    );

    let galley = painter.layout_no_wrap(label.to_string(), FontId::proportional(13.0), fg);
    let icon_w = if icon.is_some() { 24.0 } else { 0.0 };
    let total = icon_w + galley.size().x;
    let start_x = rect.center().x - total / 2.0;

    if let Some(icon) = icon {
        icons::paint(
            painter,
            Rect::from_center_size(pos2(start_x + 8.0, rect.center().y), Vec2::splat(16.0)),
            icon,
            fg,
        );
    }

    painter.galley(
        pos2(start_x + icon_w, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
}

fn record_button(
    ui: &mut egui::Ui,
    diameter: f32,
    enabled: bool,
    shortcut: bool,
) -> egui::Response {
    let p = pal();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), sense);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, "Start recording"));
    let hovered = response.hovered() && enabled;
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let center = rect.center();
    let radius = diameter / 2.0;
    let painter = ui.painter();
    let fill = if !enabled {
        p.toggle_off
    } else if hovered {
        p.danger_hi
    } else {
        p.danger
    };
    painter.circle_filled(
        center + vec2(0.0, 2.0),
        radius - 2.0,
        Color32::from_black_alpha(70),
    );
    painter.circle_filled(center, radius - 2.0, fill);
    painter.text(
        center,
        Align2::CENTER_CENTER,
        "rec",
        FontId::new(diameter * 0.27, theme::bold()),
        Color32::WHITE,
    );
    if response.has_focus() {
        painter.circle_stroke(center, radius + 1.0, Stroke::new(2.0, p.focus));
    }
    let tip = if !enabled {
        "Complete the setup first"
    } else if shortcut {
        "Start recording (F9)"
    } else {
        "Start recording"
    };
    response.on_hover_text(tip)
}

/// A Camtasia-style input tile: large glyph on top, switch + name below.
#[allow(clippy::too_many_arguments)]
fn source_tile(
    ui: &mut egui::Ui,
    id: &str,
    size: Vec2,
    icon: Icon,
    label: &str,
    value: &mut bool,
    available: bool,
    tooltip: &str,
) {
    let p = pal();
    let enabled = ui.is_enabled() && available;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let response = ui.interact(rect, ui.scope_id().with(id), sense);
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, *value, label));
    if response.clicked() {
        *value = !*value;
    }
    let on = *value && available;
    let hovered = response.hovered() && enabled;
    let painter = ui.painter_at(rect.expand(3.0));
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS_PANEL),
        if hovered {
            p.surface_elevated
        } else {
            p.surface
        },
        Stroke::new(1.0, if hovered { p.border_hi } else { p.border_soft }),
        StrokeKind::Inside,
    );
    let bar_h = 40.0;
    let top = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - bar_h));
    let glyph = Rect::from_center_size(top.center() - vec2(0.0, 10.0), Vec2::splat(40.0));
    let color = if on { p.text } else { p.muted_dim };
    icons::paint(&painter, glyph, icon, color);
    if !on {
        // "Off" slash across the glyph.
        let tile_fill = if hovered {
            p.surface_elevated
        } else {
            p.surface
        };
        let a = glyph.left_top() + vec2(4.0, 4.0);
        let b = glyph.right_bottom() - vec2(4.0, 4.0);
        painter.line_segment([a, b], Stroke::new(5.0, tile_fill));
        painter.line_segment([a, b], Stroke::new(2.4, color));
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_string(),
        FontId::new(13.0, theme::bold()),
        if available { p.text } else { p.muted_dim },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 16.0);
    let galley = painter.layout_job(job);
    painter.galley(
        pos2(
            rect.center().x - galley.size().x / 2.0,
            glyph.bottom() + 8.0,
        ),
        galley,
        p.text,
    );
    painter.line_segment(
        [
            pos2(rect.left() + 1.0, rect.bottom() - bar_h),
            pos2(rect.right() - 1.0, rect.bottom() - bar_h),
        ],
        Stroke::new(1.0, p.border_soft),
    );
    let switch = Rect::from_min_size(
        pos2(rect.left() + 10.0, rect.bottom() - bar_h / 2.0 - 10.0),
        vec2(36.0, 20.0),
    );
    paint_toggle(ui.ctx(), &painter, response.id, switch, on, hovered);
    let state = if !available {
        "Unavailable"
    } else if on {
        "On"
    } else {
        "Off"
    };
    painter.text(
        pos2(switch.right() + 10.0, switch.center().y),
        Align2::LEFT_CENTER,
        state,
        FontId::proportional(12.5),
        if on { p.text_soft } else { p.muted },
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(theme::RADIUS_PANEL + 2),
            Stroke::new(2.0, p.focus),
            StrokeKind::Outside,
        );
    }
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.on_hover_text(tooltip);
}

/// Box-filtered downscale to at most `max_width` pixels wide.
fn downscale(frame: &PreviewFrame, max_width: u32) -> PreviewFrame {
    if frame.width <= max_width || frame.width == 0 {
        return PreviewFrame {
            width: frame.width,
            height: frame.height,
            rgba: frame.rgba.clone(),
        };
    }
    let factor = frame.width.div_ceil(max_width);
    let width = frame.width / factor;
    let height = (frame.height / factor).max(1);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let stride = frame.width as usize * 4;
    for y in 0..height as usize {
        for x in 0..width as usize {
            let mut sum = [0u32; 4];
            for dy in 0..factor as usize {
                let row = (y * factor as usize + dy) * stride;
                for dx in 0..factor as usize {
                    let i = row + (x * factor as usize + dx) * 4;
                    if let Some(px) = frame.rgba.get(i..i + 4) {
                        for (total, value) in sum.iter_mut().zip(px) {
                            *total += *value as u32;
                        }
                    }
                }
            }
            let n = factor * factor;
            let o = (y * width as usize + x) * 4;
            for c in 0..4 {
                rgba[o + c] = (sum[c] / n) as u8;
            }
        }
    }
    PreviewFrame {
        width,
        height,
        rgba,
    }
}

fn level_meter(ui: &mut egui::Ui, level: f32, width: f32) {
    let p = pal();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 7.0), Sense::hover());
    let radius = CornerRadius::same(3);
    ui.painter().rect_filled(rect, radius, p.border_soft);

    let fill = rect.width() * level.clamp(0.0, 1.0);
    if fill > 0.5 {
        let color = if level > 0.85 {
            p.danger
        } else if level > 0.6 {
            p.caution
        } else {
            p.success
        };
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, vec2(fill, rect.height())),
            radius,
            color,
        );
    }
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    let p = pal();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(label).size(11.5).color(p.muted_dim));
        ui.label(
            RichText::new(value)
                .size(14.5)
                .family(crate::theme::bold())
                .color(p.text),
        );
    });
}

fn detail_row(ui: &mut egui::Ui, icon: Icon, label: &str, value: &str) {
    let p = pal();
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
        icons::paint(ui.painter(), rect, icon, p.muted);
        ui.label(RichText::new(label).size(12.0).color(p.muted));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(value).size(12.0).color(p.text_soft)).truncate())
                .on_hover_text(value);
        });
    });
}

pub(crate) fn empty_state(ui: &mut egui::Ui, icon: Icon, title: &str, detail: &str) {
    let p = pal();
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), p.surface_elevated);
        icons::paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(18.0)),
            icon,
            p.muted,
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(title)
                .size(13.0)
                .family(crate::theme::bold())
                .color(p.text_soft),
        );
        ui.label(RichText::new(detail).size(12.0).color(p.muted));
    });
    ui.add_space(8.0);
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

pub(crate) trait HoverIf {
    fn on_hover_text_if(self, show: bool, text: &str) -> Self;
}

impl HoverIf for egui::Response {
    fn on_hover_text_if(self, show: bool, text: &str) -> Self {
        if show {
            self.on_hover_text(text)
        } else {
            self
        }
    }
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn non_empty_or(value: String, fallback: fn() -> PathBuf) -> String {
    if value.trim().is_empty() {
        fallback().to_string_lossy().to_string()
    } else {
        value
    }
}

fn folder_or(value: &str, fallback: fn() -> PathBuf) -> PathBuf {
    let path = PathBuf::from(value.trim());
    if path.as_os_str().is_empty() {
        fallback()
    } else {
        path
    }
}

fn load_prefs() -> Prefs {
    let Some(dir) = util::config_dir() else {
        return Prefs::default();
    };
    std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn load_logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([256, 256], WINDOW_ICON);
    ctx.load_texture("scroder_logo", image, egui::TextureOptions::LINEAR)
}

pub(crate) fn fmt_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

const WINDOW_ICON: &[u8] = include_bytes!("../resources/icon_256.rgba");

fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: WINDOW_ICON.to_vec(),
        width: 256,
        height: 256,
    }
}

pub fn run(open: Vec<PathBuf>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(crate::APP_NAME)
            .with_app_id("scroder")
            .with_icon(window_icon())
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([980.0, 640.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        crate::APP_NAME,
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            for path in open {
                app.editor.open(path, true, &cc.egui_ctx);
                app.page = Page::Edit;
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
}
