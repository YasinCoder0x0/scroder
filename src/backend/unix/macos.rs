//! macOS specifics: CoreGraphics display/window enumeration, AVFoundation
//! devices (through ffmpeg), the Screen Recording permission and
//! `screencapture` for screenshots.

use std::ffi::{c_char, c_void};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context as _, Result};

use super::{decode_image, ffmpeg, PreparedVideo, VideoInput};
use crate::media::{
    Capabilities, CaptureTarget, DisplaySource, MissingDependency, PreviewFrame, RecordingOptions,
    Region, WindowSource,
};

// ---------------------------------------------------------------------------
// CoreGraphics / CoreFoundation FFI
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CFTypeRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetRefreshRate(mode: *mut c_void) -> f64;
    fn CGDisplayModeRelease(mode: *mut c_void);
    fn CGDisplayIsBuiltin(display: u32) -> u32;
    fn CGPreflightScreenCaptureAccess() -> u8;
    fn CGRequestScreenCaptureAccess() -> u8;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> u8;

    static kCGWindowNumber: CFTypeRef;
    static kCGWindowLayer: CFTypeRef;
    static kCGWindowBounds: CFTypeRef;
    static kCGWindowName: CFTypeRef;
    static kCGWindowOwnerName: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
    fn CFStringGetLength(string: CFTypeRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(string: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32)
        -> u8;
    fn CFRelease(object: CFTypeRef);
}

const K_CF_NUMBER_SINT64: isize = 4;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const ON_SCREEN_ONLY: u32 = 1 << 0;
const INCLUDING_WINDOW: u32 = 1 << 3;
const EXCLUDE_DESKTOP: u32 = 1 << 4;

unsafe fn cf_string(value: CFTypeRef) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let length = CFStringGetLength(value);
    let size = CFStringGetMaximumSizeForEncoding(length, K_CF_STRING_ENCODING_UTF8) + 1;
    let mut buffer = vec![0u8; size.max(1) as usize];
    if CFStringGetCString(
        value,
        buffer.as_mut_ptr() as *mut c_char,
        size,
        K_CF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    Some(String::from_utf8_lossy(&buffer[..end]).to_string())
}

unsafe fn cf_i64(value: CFTypeRef) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    let mut out: i64 = 0;
    (CFNumberGetValue(
        value,
        K_CF_NUMBER_SINT64,
        &mut out as *mut i64 as *mut c_void,
    ) != 0)
        .then_some(out)
}

struct WindowInfo {
    id: u32,
    title: String,
    app: String,
    bounds: CGRect,
}

fn window_infos(option: u32, relative_to: u32) -> Vec<WindowInfo> {
    unsafe {
        let array = CGWindowListCopyWindowInfo(option, relative_to);
        if array.is_null() {
            return Vec::new();
        }
        let mut list = Vec::new();
        for i in 0..CFArrayGetCount(array) {
            let dict = CFArrayGetValueAtIndex(array, i);
            if dict.is_null() {
                continue;
            }
            let layer = cf_i64(CFDictionaryGetValue(dict, kCGWindowLayer)).unwrap_or(-1);
            if layer != 0 {
                continue;
            }
            let Some(id) = cf_i64(CFDictionaryGetValue(dict, kCGWindowNumber)) else {
                continue;
            };
            let mut bounds = CGRect::default();
            let bounds_dict = CFDictionaryGetValue(dict, kCGWindowBounds);
            if bounds_dict.is_null()
                || CGRectMakeWithDictionaryRepresentation(bounds_dict, &mut bounds) == 0
            {
                continue;
            }
            let app = cf_string(CFDictionaryGetValue(dict, kCGWindowOwnerName)).unwrap_or_default();
            let title = cf_string(CFDictionaryGetValue(dict, kCGWindowName)).unwrap_or_default();
            list.push(WindowInfo {
                id: id as u32,
                title,
                app,
                bounds,
            });
        }
        CFRelease(array);
        list
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

fn has_screen_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() != 0 }
}

pub fn base_capabilities() -> Capabilities {
    let mut missing = Vec::new();
    if !has_screen_permission() {
        // Shows the system prompt once and adds ScreenRec to the settings list.
        let granted = unsafe { CGRequestScreenCaptureAccess() != 0 };
        if !granted {
            missing.push(MissingDependency {
                title: "Screen Recording permission needed".into(),
                detail: "Allow ScreenRec in System Settings › Privacy & Security › Screen & \
                         System Audio Recording, then quit and reopen ScreenRec."
                    .into(),
                install_command: None,
            });
        }
    }

    let loopback = loopback_device().is_some();
    Capabilities {
        platform: "macOS",
        encoder_label: String::new(),
        window_capture: true,
        region_capture: true,
        portal_source: false,
        system_audio: loopback,
        system_audio_note: (!loopback).then(|| {
            "macOS cannot record system audio directly. Install a free loopback driver such as \
             BlackHole, route your output through it, then click Check again."
                .to_string()
        }),
        hevc: false,
        global_hotkeys: true,
        hotkey_note: Some("On compact keyboards hold fn while pressing F9, F10 or F11.".into()),
        missing,
    }
}

pub fn ffmpeg_missing() -> MissingDependency {
    MissingDependency {
        title: "FFmpeg is required".into(),
        detail: "ScreenRec uses FFmpeg to capture and encode on macOS. Install it with Homebrew \
                 (https://brew.sh), then click Check again."
            .into(),
        install_command: Some("brew install ffmpeg".into()),
    }
}

pub fn explain_ffmpeg_error(tail: &str) -> String {
    let lower = tail.to_ascii_lowercase();
    if !has_screen_permission() || lower.contains("not authorized") || lower.contains("permission")
    {
        "ScreenRec is not allowed to record the screen. Enable it in System Settings › Privacy & \
         Security › Screen & System Audio Recording, then reopen ScreenRec."
            .into()
    } else if lower.contains("framerate") && lower.contains("not supported") {
        format!("The camera does not support 30 fps. Turn off the webcam overlay.\n{tail}")
    } else {
        tail.to_string()
    }
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

pub fn list_displays() -> Vec<DisplaySource> {
    unsafe {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) != 0 {
            return Vec::new();
        }
        let main = CGMainDisplayID();
        let mut external = 0;
        ids[..count as usize]
            .iter()
            .enumerate()
            .map(|(index, &id)| {
                let bounds = CGDisplayBounds(id);
                let mode = CGDisplayCopyDisplayMode(id);
                let (pixel_w, pixel_h, hz) = if mode.is_null() {
                    (bounds.size.width as u32, bounds.size.height as u32, 0)
                } else {
                    let values = (
                        CGDisplayModeGetPixelWidth(mode) as u32,
                        CGDisplayModeGetPixelHeight(mode) as u32,
                        CGDisplayModeGetRefreshRate(mode).round() as u32,
                    );
                    CGDisplayModeRelease(mode);
                    values
                };
                let scale = if bounds.size.width > 0.0 {
                    (pixel_w as f64 / bounds.size.width) as f32
                } else {
                    1.0
                };
                let name = if CGDisplayIsBuiltin(id) != 0 {
                    "Built-in Display".to_string()
                } else {
                    external += 1;
                    format!("Display {external}")
                };
                DisplaySource {
                    id: id as u64,
                    index,
                    name,
                    x: (bounds.origin.x * scale as f64).round() as i32,
                    y: (bounds.origin.y * scale as f64).round() as i32,
                    width: pixel_w,
                    height: pixel_h,
                    scale,
                    // ProMotion and some external displays report 0.
                    refresh_hz: if hz == 0 { 60 } else { hz },
                    primary: id == main,
                }
            })
            .collect()
    }
}

pub fn list_windows() -> Vec<WindowSource> {
    let own_pid_app = crate::APP_NAME;
    let displays = list_displays();
    window_infos(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0)
        .into_iter()
        .filter(|w| w.bounds.size.width > 40.0 && w.bounds.size.height > 40.0)
        .filter(|w| w.app != own_pid_app)
        .map(|w| {
            let scale = display_for(&displays, &w.bounds)
                .map(|d| d.scale)
                .unwrap_or(1.0) as f64;
            let title = if w.title.trim().is_empty() {
                w.app.clone()
            } else {
                w.title.clone()
            };
            WindowSource {
                id: w.id as u64,
                title,
                app: w.app,
                width: (w.bounds.size.width * scale).round() as u32,
                height: (w.bounds.size.height * scale).round() as u32,
            }
        })
        .collect()
}

fn display_for<'a>(displays: &'a [DisplaySource], rect: &CGRect) -> Option<&'a DisplaySource> {
    let cx = rect.origin.x + rect.size.width / 2.0;
    let cy = rect.origin.y + rect.size.height / 2.0;
    displays
        .iter()
        .find(|d| {
            let (x, y) = d.logical_position();
            let (w, h) = d.logical_size();
            cx >= x as f64 && cx < (x + w) as f64 && cy >= y as f64 && cy < (y + h) as f64
        })
        .or_else(|| displays.iter().find(|d| d.primary))
        .or_else(|| displays.first())
}

// ---------------------------------------------------------------------------
// AVFoundation devices
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AvDevices {
    video: Vec<(usize, String)>,
    audio: Vec<(usize, String)>,
}

fn av_devices() -> AvDevices {
    let Some(ffmpeg) = ffmpeg::locate() else {
        return AvDevices::default();
    };
    let Ok(out) = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .stdin(Stdio::null())
        .output()
    else {
        return AvDevices::default();
    };

    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let mut devices = AvDevices::default();
    let mut audio = false;
    for line in text.lines() {
        if line.contains("AVFoundation video devices") {
            audio = false;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            audio = true;
            continue;
        }
        // "[AVFoundation indev @ 0x...] [0] FaceTime HD Camera"
        let rest = match line.find("] [") {
            Some(pos) => &line[pos + 2..],
            None => continue,
        };
        let Some(close) = rest.find(']') else {
            continue;
        };
        let Ok(index) = rest[1..close].trim().parse::<usize>() else {
            continue;
        };
        let name = rest[close + 1..].trim().to_string();
        if audio {
            devices.audio.push((index, name));
        } else {
            devices.video.push((index, name));
        }
    }
    devices
}

fn loopback_device() -> Option<usize> {
    av_devices()
        .audio
        .into_iter()
        .find(|(_, name)| {
            let lower = name.to_ascii_lowercase();
            ["blackhole", "loopback", "soundflower", "background music"]
                .iter()
                .any(|needle| lower.contains(needle))
        })
        .map(|(index, _)| index)
}

fn cameras() -> Vec<(usize, String)> {
    av_devices()
        .video
        .into_iter()
        .filter(|(_, name)| !name.starts_with("Capture screen"))
        .collect()
}

pub fn list_cameras() -> Vec<String> {
    cameras().into_iter().map(|(_, name)| name).collect()
}

pub fn camera_input(index: usize) -> Option<Vec<String>> {
    let (device, _) = cameras().into_iter().nth(index)?;
    Some(
        [
            "-thread_queue_size",
            "512",
            "-f",
            "avfoundation",
            "-framerate",
            "30",
            "-i",
            &format!("{device}:none"),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    )
}

fn avfoundation_audio(device: &str) -> Vec<String> {
    [
        "-thread_queue_size",
        "1024",
        "-f",
        "avfoundation",
        "-i",
        &format!(":{device}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn audio_inputs(system: bool, microphone: bool) -> (Vec<Vec<String>>, Vec<String>) {
    let mut inputs = Vec::new();
    let mut warnings = Vec::new();
    if system {
        match loopback_device() {
            Some(index) => inputs.push(avfoundation_audio(&index.to_string())),
            None => warnings.push(
                "System audio was skipped: install a loopback driver such as BlackHole to record it."
                    .into(),
            ),
        }
    }
    if microphone {
        inputs.push(avfoundation_audio("default"));
    }
    (inputs, warnings)
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

fn screen_device(display: &DisplaySource) -> Result<usize> {
    let wanted = format!("Capture screen {}", display.index);
    av_devices()
        .video
        .into_iter()
        .find(|(_, name)| *name == wanted)
        .map(|(index, _)| index)
        .ok_or_else(|| {
            anyhow!("FFmpeg cannot see \"{wanted}\"; check the Screen Recording permission")
        })
}

/// Pixel crop of a window on its display.
fn window_crop(window_id: u64) -> Result<(DisplaySource, Region)> {
    let info = window_infos(INCLUDING_WINDOW, window_id as u32)
        .into_iter()
        .find(|w| w.id as u64 == window_id)
        .ok_or_else(|| {
            anyhow!("the selected window is no longer available; refresh the window list")
        })?;
    let displays = list_displays();
    let display = display_for(&displays, &info.bounds)
        .cloned()
        .ok_or_else(|| anyhow!("no display found for the selected window"))?;
    let (dx, dy) = display.logical_position();
    let scale = display.scale as f64;
    let x = ((info.bounds.origin.x - dx as f64) * scale)
        .max(0.0)
        .round() as u32;
    let y = ((info.bounds.origin.y - dy as f64) * scale)
        .max(0.0)
        .round() as u32;
    let region = Region {
        x,
        y,
        width: (info.bounds.size.width * scale).round() as u32,
        height: (info.bounds.size.height * scale).round() as u32,
    }
    .normalized(display.width, display.height)
    .ok_or_else(|| anyhow!("the selected window is not visible on screen"))?;
    Ok((display, region))
}

fn ensure_permission() -> Result<()> {
    if has_screen_permission() {
        return Ok(());
    }
    unsafe {
        CGRequestScreenCaptureAccess();
    }
    bail!(
        "ScreenRec needs the Screen Recording permission. Enable it in System Settings › Privacy & \
         Security › Screen & System Audio Recording, then reopen ScreenRec."
    )
}

pub fn prepare_video(options: &RecordingOptions) -> Result<PreparedVideo> {
    ensure_permission()?;
    let (display, crop, warnings) = match &options.target {
        CaptureTarget::Display(display) => (
            display.clone(),
            options
                .region
                .and_then(|r| r.normalized(display.width, display.height)),
            Vec::new(),
        ),
        CaptureTarget::Window(window) => {
            let (display, region) = window_crop(window.id)?;
            (
                display,
                Some(region),
                vec![
                    "Window recording captures the window's screen area; keep it in place."
                        .to_string(),
                ],
            )
        }
        CaptureTarget::Portal { .. } => bail!("portal capture is not available on macOS"),
    };
    let device_index = screen_device(&display)?;
    Ok(PreparedVideo {
        input: VideoInput::AvFoundation { device_index },
        out_height: crop.map(|c| c.height).unwrap_or(display.height),
        crop,
        session: None,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Screenshots and previews
// ---------------------------------------------------------------------------

fn screencapture() -> Command {
    let mut command = Command::new("/usr/sbin/screencapture");
    command.args(["-x", "-t", "png"]).stdin(Stdio::null());
    command
}

fn run_screencapture(mut command: Command) -> Result<()> {
    let out = command.output().context("failed to run screencapture")?;
    if !out.status.success() {
        bail!(
            "screencapture failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn screenshot(target: &CaptureTarget, region: Option<Region>, output: &Path) -> Result<()> {
    ensure_permission()?;
    let mut command = screencapture();
    match target {
        CaptureTarget::Display(display) => {
            match region.and_then(|r| r.normalized(display.width, display.height)) {
                Some(r) => {
                    let (dx, dy) = display.logical_position();
                    let scale = display.scale.max(0.1);
                    command.arg(format!(
                        "-R{},{},{},{}",
                        dx + r.x as f32 / scale,
                        dy + r.y as f32 / scale,
                        r.width as f32 / scale,
                        r.height as f32 / scale
                    ));
                }
                None => {
                    command.arg(format!("-D{}", display.index + 1));
                }
            }
        }
        CaptureTarget::Window(window) => {
            command.arg("-o").arg(format!("-l{}", window.id));
        }
        CaptureTarget::Portal { .. } => bail!("portal capture is not available on macOS"),
    }
    command.arg(output);
    run_screencapture(command)
}

pub fn grab(target: &CaptureTarget) -> Result<PreviewFrame> {
    let CaptureTarget::Display(display) = target else {
        bail!("previews are only available for displays");
    };
    let temp = std::env::temp_dir().join(format!("screenrec-preview-{}.png", std::process::id()));
    let mut command = screencapture();
    command.arg(format!("-D{}", display.index + 1)).arg(&temp);
    run_screencapture(command)?;
    let frame = decode_image(&temp, display.width, display.height);
    let _ = std::fs::remove_file(&temp);
    frame
}
