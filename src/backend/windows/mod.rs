//! Native Windows backend: Windows Graphics Capture for video, Media
//! Foundation for hardware encoding and webcams, WASAPI for audio.

mod audio;
mod capture;
mod monitor_util;
mod webcam;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use parking_lot::Mutex;
use windows_capture::capture::{
    CaptureControl, GraphicsCaptureApiError, GraphicsCaptureApiHandler,
};
use windows_capture::encoder::VideoEncoder;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use self::audio::AudioHandles;
use self::capture::{
    CaptureFlags, CaptureHandler, GrabFlags, GrabFrame, GrabHandler, ScreenshotFlags,
    ScreenshotHandler,
};
use self::webcam::WebcamStream;
use crate::media::{
    Capabilities, CaptureTarget, DisplaySource, PauseClock, PreviewFrame, RecordingOptions, Region,
    WindowSource,
};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type SharedEncoder = Arc<Mutex<Option<VideoEncoder>>>;
pub type SharedError = Arc<Mutex<Option<String>>>;
type Control = CaptureControl<CaptureHandler, BoxError>;
type ScreenshotControl = CaptureControl<ScreenshotHandler, BoxError>;
type GrabControl = CaptureControl<GrabHandler, BoxError>;

#[derive(Clone, Copy)]
enum NativeTarget {
    Monitor(Monitor),
    Window(Window),
}

impl NativeTarget {
    fn from_target(target: &CaptureTarget) -> Result<Self> {
        match target {
            CaptureTarget::Display(display) => Ok(NativeTarget::Monitor(
                Monitor::from_raw_hmonitor(display.id as usize as *mut std::ffi::c_void),
            )),
            CaptureTarget::Window(window) => {
                let handle = Window::from_raw_hwnd(window.id as usize as *mut std::ffi::c_void);
                if !handle.is_valid() {
                    bail!("the selected window is no longer available; refresh the window list");
                }
                Ok(NativeTarget::Window(handle))
            }
            CaptureTarget::Portal { .. } => bail!("portal capture is not available on Windows"),
        }
    }

    fn width(&self) -> u32 {
        match self {
            NativeTarget::Monitor(m) => m.width().unwrap_or(0),
            NativeTarget::Window(w) => w.width().unwrap_or(0).max(0) as u32,
        }
    }

    fn height(&self) -> u32 {
        match self {
            NativeTarget::Monitor(m) => m.height().unwrap_or(0),
            NativeTarget::Window(w) => w.height().unwrap_or(0).max(0) as u32,
        }
    }
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        platform: "Windows",
        encoder_label: "Hardware accelerated (Media Foundation)".into(),
        window_capture: true,
        region_capture: true,
        portal_source: false,
        system_audio: true,
        system_audio_note: None,
        hevc: true,
        global_hotkeys: true,
        hotkey_note: None,
        missing: Vec::new(),
    }
}

pub fn list_displays() -> Vec<DisplaySource> {
    let bounds = monitor_util::enumerate();
    Monitor::enumerate()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, handle)| {
            let device_name = handle.device_name().unwrap_or_default();
            let name = handle.name().unwrap_or_default();
            let name = if name.trim().is_empty() {
                format!("Display {}", handle.index().unwrap_or(index + 1))
            } else {
                name
            };
            let bound = bounds
                .iter()
                .find(|b| b.device_name.eq_ignore_ascii_case(&device_name));
            DisplaySource {
                id: handle.as_raw_hmonitor() as usize as u64,
                index,
                name,
                x: bound.map(|b| b.x).unwrap_or(0),
                y: bound.map(|b| b.y).unwrap_or(0),
                width: handle.width().unwrap_or(0),
                height: handle.height().unwrap_or(0),
                scale: bound.map(|b| b.scale).unwrap_or(1.0),
                refresh_hz: handle.refresh_rate().unwrap_or(60),
                primary: bound.is_some_and(|b| b.primary),
            }
        })
        .collect()
}

pub fn list_windows() -> Vec<WindowSource> {
    Window::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|handle| {
            if !handle.is_valid() {
                return None;
            }
            let width = handle.width().unwrap_or(0);
            let height = handle.height().unwrap_or(0);
            // Minimized windows report a tiny caption-sized rect and capture as black.
            if width < 120 || height < 60 {
                return None;
            }
            let title = handle.title().ok()?;
            let title = title.trim().to_string();
            if title.is_empty()
                || title == crate::APP_NAME
                || title == "Program Manager"
                || title == "Windows Input Experience"
            {
                return None;
            }
            let app = handle
                .process_name()
                .unwrap_or_default()
                .trim_end_matches(".exe")
                .trim_end_matches(".EXE")
                .to_string();
            Some(WindowSource {
                id: handle.as_raw_hwnd() as usize as u64,
                title,
                app,
                width: width as u32,
                height: height as u32,
            })
        })
        .collect()
}

pub fn list_cameras() -> Vec<String> {
    webcam::enumerate_devices()
}

/// FFmpeg is only needed on Windows for the video editor.
pub fn ffmpeg_missing() -> crate::media::MissingDependency {
    crate::media::MissingDependency {
        title: "FFmpeg is required for editing".into(),
        detail: "The editor uses FFmpeg to preview and export video. Install it with the command below (or from ffmpeg.org), then click Check again."
            .into(),
        install_command: Some("winget install --id Gyan.FFmpeg -e".into()),
    }
}

pub struct ActiveRecording {
    control: Option<Control>,
    audio: Option<AudioHandles>,
    encoder: SharedEncoder,
    error: SharedError,
    frames: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    audio_level: Arc<AtomicU32>,
    audio_enabled: bool,
    clock: PauseClock,
    output_path: PathBuf,
}

impl ActiveRecording {
    /// Recorded time, excluding pauses.
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed()
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    pub fn file_size(&self) -> u64 {
        std::fs::metadata(&self.output_path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    pub fn is_capture_alive(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| !control.is_finished())
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.clock.pause();
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.clock.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Peak audio level in percent, when audio is being recorded.
    pub fn audio_level(&self) -> Option<u32> {
        self.audio_enabled
            .then(|| self.audio_level.load(Ordering::Relaxed))
    }

    pub fn stop(mut self) -> Result<PathBuf> {
        self.finalize()?;
        Ok(self.output_path.clone())
    }

    fn finalize(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);

        if let Some(audio) = self.audio.take() {
            audio.stop_and_join();
        }

        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }

        if let Some(encoder) = self.encoder.lock().take() {
            finish_encoder(encoder)?;
        }

        Ok(())
    }
}

impl Drop for ActiveRecording {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

fn finish_encoder(encoder: VideoEncoder) -> Result<()> {
    let handle = std::thread::Builder::new()
        .name("screenrec-finalize".to_string())
        .spawn(move || {
            let _ = wasapi::initialize_mta();
            encoder.finish().map_err(|err| anyhow::anyhow!("{err}"))
        })
        .context("failed to spawn finalizer thread")?;

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("encoder finalizer panicked"))?
}

pub fn start(options: RecordingOptions) -> Result<ActiveRecording> {
    let target = NativeTarget::from_target(&options.target)?;
    let target_width = target.width();
    let target_height = target.height();

    if target_width == 0 || target_height == 0 {
        bail!("the selected capture target has no size");
    }

    let region = match options.region {
        Some(region) => Some(
            region
                .normalized(target_width, target_height)
                .ok_or_else(|| anyhow::anyhow!("the selected capture region is invalid"))?,
        ),
        None => None,
    };

    let width = region.map(|r| r.width).unwrap_or(target_width);
    let height = region.map(|r| r.height).unwrap_or(target_height);

    if let Some(parent) = options.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let encoder: SharedEncoder = Arc::new(Mutex::new(None));
    let error: SharedError = Arc::new(Mutex::new(None));
    let frames = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let audio_level = Arc::new(AtomicU32::new(0));

    let audio_enabled = options.system_audio || options.microphone;

    let camera: Option<WebcamStream> = match &options.webcam {
        Some(cam_options) => match webcam::start(cam_options.clone()) {
            Ok(stream) => Some(stream),
            Err(err) => {
                *error.lock() = Some(format!("Webcam unavailable: {err:#}"));
                None
            }
        },
        None => None,
    };

    let flags = CaptureFlags {
        encoder_slot: encoder.clone(),
        error_slot: error.clone(),
        frames: frames.clone(),
        stop: stop.clone(),
        paused: paused.clone(),
        width,
        height,
        fps: options.fps,
        bitrate: options.bitrate,
        audio: audio_enabled,
        codec: options.codec,
        output_path: options.output.clone(),
        region,
        webcam: camera.clone(),
    };

    let control = start_control(target, flags, options.capture_cursor)?;

    if encoder.lock().is_none() {
        bail!("the video encoder failed to initialize");
    }

    let audio = if audio_enabled {
        Some(audio::start(
            encoder.clone(),
            error.clone(),
            options.system_audio,
            options.microphone,
            paused.clone(),
            audio_level.clone(),
        ))
    } else {
        None
    };

    Ok(ActiveRecording {
        control: Some(control),
        audio,
        encoder,
        error,
        frames,
        stop,
        paused,
        audio_level,
        audio_enabled,
        clock: PauseClock::new(),
        output_path: options.output,
    })
}

fn start_control(target: NativeTarget, flags: CaptureFlags, cursor: bool) -> Result<Control> {
    let cursor_setting = if cursor {
        CursorCaptureSettings::WithCursor
    } else {
        CursorCaptureSettings::WithoutCursor
    };

    match target {
        NativeTarget::Monitor(monitor) => start_with_fallback(monitor, flags, cursor_setting),
        NativeTarget::Window(window) => start_with_fallback(window, flags, cursor_setting),
    }
}

fn start_with_fallback<T>(
    item: T,
    flags: CaptureFlags,
    cursor: CursorCaptureSettings,
) -> Result<Control>
where
    T: Copy + TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let attempt = |cursor: CursorCaptureSettings,
                   border: DrawBorderSettings|
     -> std::result::Result<Control, GraphicsCaptureApiError<BoxError>> {
        CaptureHandler::start_free_threaded(Settings::new(
            item,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags.clone(),
        ))
    };

    match attempt(cursor, DrawBorderSettings::WithoutBorder) {
        Ok(control) => Ok(control),
        Err(first) => attempt(CursorCaptureSettings::Default, DrawBorderSettings::Default)
            .map_err(|second| anyhow::anyhow!("capture failed: {first} / fallback: {second}")),
    }
}

/// Captures a single frame of the target (optionally cropped to a region) and
/// writes it to `output` as a PNG.
pub fn screenshot(
    target: &CaptureTarget,
    region: Option<Region>,
    output: PathBuf,
) -> Result<PathBuf> {
    let target = NativeTarget::from_target(target)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let result: Arc<Mutex<Option<std::result::Result<PathBuf, String>>>> =
        Arc::new(Mutex::new(None));

    let region = region.and_then(|r| r.normalized(target.width(), target.height()));

    let flags = ScreenshotFlags {
        output: output.clone(),
        region,
        result: result.clone(),
    };

    let control = match target {
        NativeTarget::Monitor(monitor) => screenshot_with_fallback(monitor, flags),
        NativeTarget::Window(window) => screenshot_with_fallback(window, flags),
    }?;
    let _ = control.wait();

    let outcome = result.lock().take();
    match outcome {
        Some(Ok(path)) => Ok(path),
        Some(Err(err)) => bail!("{err}"),
        None => bail!("the screenshot could not be captured"),
    }
}

fn screenshot_with_fallback<T>(item: T, flags: ScreenshotFlags) -> Result<ScreenshotControl>
where
    T: Copy + TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let attempt =
        |cursor: CursorCaptureSettings,
         border: DrawBorderSettings|
         -> std::result::Result<ScreenshotControl, GraphicsCaptureApiError<BoxError>> {
            ScreenshotHandler::start_free_threaded(Settings::new(
                item,
                cursor,
                border,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                flags.clone(),
            ))
        };

    match attempt(
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
    ) {
        Ok(control) => Ok(control),
        Err(first) => attempt(CursorCaptureSettings::Default, DrawBorderSettings::Default)
            .map_err(|second| anyhow::anyhow!("screenshot failed: {first} / fallback: {second}")),
    }
}

/// Capture a single frame as an RGBA snapshot (used for the region picker).
pub fn grab(target: &CaptureTarget) -> Result<PreviewFrame> {
    let target = NativeTarget::from_target(target)?;
    let result: Arc<Mutex<Option<std::result::Result<GrabFrame, String>>>> =
        Arc::new(Mutex::new(None));

    let flags = GrabFlags {
        region: None,
        result: result.clone(),
    };

    let control = match target {
        NativeTarget::Monitor(monitor) => grab_with_fallback(monitor, flags),
        NativeTarget::Window(window) => grab_with_fallback(window, flags),
    }?;
    let _ = control.wait();

    let outcome = result.lock().take();
    match outcome {
        Some(Ok(frame)) => {
            let mut rgba = frame.bgra;
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Ok(PreviewFrame {
                width: frame.width,
                height: frame.height,
                rgba,
            })
        }
        Some(Err(err)) => bail!("{err}"),
        None => bail!("the preview could not be captured"),
    }
}

fn grab_with_fallback<T>(item: T, flags: GrabFlags) -> Result<GrabControl>
where
    T: Copy + TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let attempt = |cursor: CursorCaptureSettings,
                   border: DrawBorderSettings|
     -> std::result::Result<GrabControl, GraphicsCaptureApiError<BoxError>> {
        GrabHandler::start_free_threaded(Settings::new(
            item,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags.clone(),
        ))
    };

    match attempt(
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
    ) {
        Ok(control) => Ok(control),
        Err(first) => attempt(CursorCaptureSettings::Default, DrawBorderSettings::Default)
            .map_err(|second| anyhow::anyhow!("preview failed: {first} / fallback: {second}")),
    }
}

/// Dark title bar whose caption colour matches the app header (Windows 10 20H1+ / 11).
pub fn style_title_bar(title: &str, rgb: [u8; 3]) {
    use windows::core::HSTRING;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    unsafe {
        let Ok(hwnd) = FindWindowW(None, &HSTRING::from(title)) else {
            return;
        };
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const std::ffi::c_void,
            4,
        );
        let color: u32 = rgb[0] as u32 | (rgb[1] as u32) << 8 | (rgb[2] as u32) << 16;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &color as *const u32 as *const std::ffi::c_void,
            4,
        );
    }
}

/// Hides a window from screen capture (Windows 10 2004+), so on-screen
/// controls never appear in recordings. Returns true once applied.
pub fn exclude_from_capture(title: &str) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    unsafe {
        match FindWindowW(None, &HSTRING::from(title)) {
            Ok(hwnd) => SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok(),
            Err(_) => false,
        }
    }
}

pub fn set_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
