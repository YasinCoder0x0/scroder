//! Linux specifics: X11 enumeration, PulseAudio / PipeWire audio, V4L2
//! cameras and distribution aware install hints.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context as _, Result};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

use super::{ffmpeg, portal, PreparedVideo, VideoInput};
use crate::media::{
    Capabilities, CaptureTarget, DisplaySource, MissingDependency, PreviewFrame, RecordingOptions,
    Region, WindowSource,
};

pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
}

fn x_display() -> String {
    std::env::var("DISPLAY")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| ":0".to_string())
}

pub fn base_capabilities() -> Capabilities {
    let wayland = is_wayland();
    let mut missing = Vec::new();
    if wayland {
        missing.extend(portal::missing_dependencies());
    }
    Capabilities {
        platform: if wayland { "Linux · Wayland" } else { "Linux · X11" },
        encoder_label: String::new(),
        window_capture: true,
        region_capture: !wayland,
        portal_source: wayland,
        system_audio: true,
        system_audio_note: None,
        hevc: false,
        global_hotkeys: !wayland,
        hotkey_note: wayland.then(|| {
            "Wayland does not allow global shortcuts. F9, F10 and F11 work while ScreenRec is focused."
                .to_string()
        }),
        missing,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Distro {
    Debian,
    Fedora,
    Arch,
    Suse,
    Unknown,
}

fn distro() -> Distro {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut ids = String::new();
    for line in text.lines() {
        if let Some(value) = line
            .strip_prefix("ID=")
            .or_else(|| line.strip_prefix("ID_LIKE="))
        {
            ids.push(' ');
            ids.push_str(&value.trim_matches('"').to_ascii_lowercase());
        }
    }
    let has = |name: &str| ids.split_whitespace().any(|id| id == name);
    if has("debian") || has("ubuntu") {
        Distro::Debian
    } else if has("fedora") || has("rhel") || has("centos") {
        Distro::Fedora
    } else if has("arch") {
        Distro::Arch
    } else if has("suse") || has("opensuse") {
        Distro::Suse
    } else {
        Distro::Unknown
    }
}

pub fn install_command(debian: &str, fedora: &str, arch: &str, suse: &str) -> Option<String> {
    match distro() {
        Distro::Debian => Some(format!("sudo apt install {debian}")),
        Distro::Fedora => Some(format!("sudo dnf install {fedora}")),
        Distro::Arch => Some(format!("sudo pacman -S {arch}")),
        Distro::Suse => Some(format!("sudo zypper install {suse}")),
        Distro::Unknown => None,
    }
}

pub fn ffmpeg_missing() -> MissingDependency {
    MissingDependency {
        title: "FFmpeg is required".into(),
        detail: "ScreenRec uses FFmpeg to capture and encode on Linux. Install it with your \
                 package manager, then click Check again."
            .into(),
        install_command: install_command("ffmpeg", "ffmpeg-free", "ffmpeg", "ffmpeg"),
    }
}

pub fn explain_ffmpeg_error(tail: &str) -> String {
    let lower = tail.to_ascii_lowercase();
    if lower.contains("cannot open display") {
        "Could not connect to the X11 display. Make sure ScreenRec runs inside your desktop session."
            .into()
    } else if lower.contains("unknown input format: 'pulse'") {
        "This FFmpeg build has no PulseAudio support, so audio cannot be recorded. \
         Turn off audio or install your distribution's FFmpeg package."
            .into()
    } else if lower.contains("pulse") && lower.contains("connection refused") {
        "Could not connect to the audio server (PulseAudio / PipeWire). Turn off audio or start \
         the audio service."
            .into()
    } else if lower.contains("video4linux") || lower.contains("/dev/video") {
        format!("The webcam could not be opened. Turn off the webcam overlay or close other apps using it.\n{tail}")
    } else {
        tail.to_string()
    }
}

// ---------------------------------------------------------------------------
// X11 enumeration
// ---------------------------------------------------------------------------

fn connect() -> Option<(RustConnection, Window)> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    Some((conn, root))
}

fn x11_scale(conn: &RustConnection) -> f32 {
    if let Some(value) = std::env::var("WINIT_X11_SCALE_FACTOR")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        return value.max(0.5);
    }
    x11rb::resource_manager::new_from_default(conn)
        .ok()
        .and_then(|db| db.get_string("Xft.dpi", "").map(str::to_string))
        .and_then(|dpi| dpi.trim().parse::<f32>().ok())
        .map(|dpi| (dpi / 96.0).max(0.5))
        .unwrap_or(1.0)
}

pub fn list_displays() -> Vec<DisplaySource> {
    if is_wayland() {
        return Vec::new();
    }
    let Some((conn, root)) = connect() else {
        return Vec::new();
    };
    let scale = x11_scale(&conn);

    let monitors = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.monitors)
        .unwrap_or_default();

    let mut list: Vec<DisplaySource> = monitors
        .into_iter()
        .enumerate()
        .map(|(index, m)| {
            let name = conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Display {}", index + 1));
            DisplaySource {
                id: index as u64,
                index,
                name,
                x: m.x as i32,
                y: m.y as i32,
                width: m.width as u32,
                height: m.height as u32,
                scale,
                refresh_hz: 0,
                primary: m.primary,
            }
        })
        .collect();

    if list.is_empty() {
        // No RandR: the whole root window is one display.
        if let Ok(Ok(geometry)) = conn.get_geometry(root).map(|c| c.reply()) {
            list.push(DisplaySource {
                id: 0,
                index: 0,
                name: "Screen".into(),
                x: 0,
                y: 0,
                width: geometry.width as u32,
                height: geometry.height as u32,
                scale,
                refresh_hz: 0,
                primary: true,
            });
        }
    }
    list
}

struct Atoms {
    client_list: u32,
    wm_name: u32,
    utf8: u32,
    state: u32,
    hidden: u32,
    window_type: u32,
    type_normal: u32,
    type_dialog: u32,
}

fn intern(conn: &RustConnection, name: &str) -> u32 {
    conn.intern_atom(false, name.as_bytes())
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom)
        .unwrap_or(0)
}

fn property_bytes(conn: &RustConnection, window: Window, prop: u32, kind: u32) -> Vec<u8> {
    conn.get_property(false, window, prop, kind, 0, 4096)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.value)
        .unwrap_or_default()
}

fn property_u32(conn: &RustConnection, window: Window, prop: u32, kind: u32) -> Vec<u32> {
    conn.get_property(false, window, prop, kind, 0, u32::MAX / 4)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(|v| v.collect()))
        .unwrap_or_default()
}

/// Absolute geometry of an X11 window in root coordinates.
fn window_geometry(
    conn: &RustConnection,
    root: Window,
    window: Window,
) -> Option<(i32, i32, u32, u32)> {
    let geometry = conn.get_geometry(window).ok()?.reply().ok()?;
    let translated = conn
        .translate_coordinates(window, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    Some((
        translated.dst_x as i32,
        translated.dst_y as i32,
        geometry.width as u32,
        geometry.height as u32,
    ))
}

pub fn list_windows() -> Vec<WindowSource> {
    if is_wayland() {
        return Vec::new();
    }
    let Some((conn, root)) = connect() else {
        return Vec::new();
    };
    let atoms = Atoms {
        client_list: intern(&conn, "_NET_CLIENT_LIST"),
        wm_name: intern(&conn, "_NET_WM_NAME"),
        utf8: intern(&conn, "UTF8_STRING"),
        state: intern(&conn, "_NET_WM_STATE"),
        hidden: intern(&conn, "_NET_WM_STATE_HIDDEN"),
        window_type: intern(&conn, "_NET_WM_WINDOW_TYPE"),
        type_normal: intern(&conn, "_NET_WM_WINDOW_TYPE_NORMAL"),
        type_dialog: intern(&conn, "_NET_WM_WINDOW_TYPE_DIALOG"),
    };

    property_u32(&conn, root, atoms.client_list, AtomEnum::WINDOW.into())
        .into_iter()
        .filter_map(|window| {
            let types = property_u32(&conn, window, atoms.window_type, AtomEnum::ATOM.into());
            if !types.is_empty()
                && !types
                    .iter()
                    .any(|t| *t == atoms.type_normal || *t == atoms.type_dialog)
            {
                return None;
            }
            let state = property_u32(&conn, window, atoms.state, AtomEnum::ATOM.into());
            if state.contains(&atoms.hidden) {
                return None;
            }

            let mut title =
                String::from_utf8_lossy(&property_bytes(&conn, window, atoms.wm_name, atoms.utf8))
                    .to_string();
            if title.trim().is_empty() {
                title = String::from_utf8_lossy(&property_bytes(
                    &conn,
                    window,
                    AtomEnum::WM_NAME.into(),
                    AtomEnum::STRING.into(),
                ))
                .to_string();
            }
            let title = title.trim().to_string();
            if title.is_empty() || title == crate::APP_NAME {
                return None;
            }

            let class = property_bytes(
                &conn,
                window,
                AtomEnum::WM_CLASS.into(),
                AtomEnum::STRING.into(),
            );
            let app = class
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .nth(1)
                .map(|part| String::from_utf8_lossy(part).to_string())
                .unwrap_or_default();

            let (_, _, width, height) = window_geometry(&conn, root, window)?;
            if width <= 10 || height <= 10 {
                return None;
            }
            Some(WindowSource {
                id: window as u64,
                title,
                app,
                width,
                height,
            })
        })
        .collect()
}

fn root_size(conn: &RustConnection, root: Window) -> (u32, u32) {
    conn.get_geometry(root)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|g| (g.width as u32, g.height as u32))
        .unwrap_or((0, 0))
}

/// Screen area covered by a window, clamped to the root window and made even.
fn window_area(window_id: u64) -> Result<(i32, i32, u32, u32)> {
    let (conn, root) = connect().ok_or_else(|| anyhow!("could not connect to the X server"))?;
    let (x, y, w, h) = window_geometry(&conn, root, window_id as Window).ok_or_else(|| {
        anyhow!("the selected window is no longer available; refresh the window list")
    })?;
    let (root_w, root_h) = root_size(&conn, root);
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w as i32).min(root_w as i32);
    let y1 = (y + h as i32).min(root_h as i32);
    let width = ((x1 - x0).max(0) as u32) & !1;
    let height = ((y1 - y0).max(0) as u32) & !1;
    if width < 2 || height < 2 {
        bail!("the selected window is not visible on screen");
    }
    Ok((x0, y0, width, height))
}

fn display_area(display: &DisplaySource, region: Option<Region>) -> (i32, i32, u32, u32) {
    match region.and_then(|r| r.normalized(display.width, display.height)) {
        Some(r) => (
            display.x + r.x as i32,
            display.y + r.y as i32,
            r.width,
            r.height,
        ),
        None => (
            display.x,
            display.y,
            display.width & !1,
            display.height & !1,
        ),
    }
}

pub fn prepare_video(options: &RecordingOptions) -> Result<PreparedVideo> {
    match &options.target {
        CaptureTarget::Portal { window } => portal::prepare(*window, options.capture_cursor),
        CaptureTarget::Display(display) => {
            let (x, y, width, height) = display_area(display, options.region);
            Ok(PreparedVideo {
                input: VideoInput::X11Area {
                    display: x_display(),
                    x,
                    y,
                    width,
                    height,
                },
                crop: None,
                out_height: height,
                session: None,
                warnings: Vec::new(),
            })
        }
        CaptureTarget::Window(window) => {
            let supports_id = ffmpeg::locate()
                .map(|path| ffmpeg::x11grab_supports_window_id(&path))
                .unwrap_or(false);
            if supports_id {
                Ok(PreparedVideo {
                    input: VideoInput::X11Window {
                        display: x_display(),
                        window_id: window.id,
                    },
                    crop: None,
                    out_height: window.height,
                    session: None,
                    warnings: Vec::new(),
                })
            } else {
                let (x, y, width, height) = window_area(window.id)?;
                Ok(PreparedVideo {
                    input: VideoInput::X11Area {
                        display: x_display(),
                        x,
                        y,
                        width,
                        height,
                    },
                    crop: None,
                    out_height: height,
                    session: None,
                    warnings: vec![
                        "This FFmpeg version records the window's screen area; keep the window in place."
                            .into(),
                    ],
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

fn ffmpeg_has_input_device(name: &str) -> bool {
    let Some(ffmpeg) = ffmpeg::locate() else {
        return false;
    };
    Command::new(ffmpeg)
        .args(["-hide_banner", "-devices"])
        .stdin(Stdio::null())
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout).lines().any(|line| {
                let mut parts = line.split_whitespace();
                matches!(parts.next(), Some(flags) if flags.contains('D'))
                    && parts.next() == Some(name)
            })
        })
        .unwrap_or(false)
}

fn default_monitor_source() -> String {
    let pactl = ffmpeg::find_tool("pactl");
    if let Some(pactl) = &pactl {
        if let Ok(out) = Command::new(pactl).arg("get-default-sink").output() {
            let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !sink.is_empty() {
                return format!("{sink}.monitor");
            }
        }
        if let Ok(out) = Command::new(pactl).arg("info").output() {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if let Some(sink) = text
                .lines()
                .find_map(|line| line.strip_prefix("Default Sink:"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return format!("{sink}.monitor");
            }
        }
    }
    "@DEFAULT_MONITOR@".to_string()
}

fn pulse_input(device: &str) -> Vec<String> {
    ["-thread_queue_size", "1024", "-f", "pulse", "-i", device]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn audio_inputs(system: bool, microphone: bool) -> (Vec<Vec<String>>, Vec<String>) {
    let mut inputs = Vec::new();
    let mut warnings = Vec::new();
    if !system && !microphone {
        return (inputs, warnings);
    }

    if ffmpeg_has_input_device("pulse") {
        if system {
            inputs.push(pulse_input(&default_monitor_source()));
        }
        if microphone {
            inputs.push(pulse_input("default"));
        }
    } else if ffmpeg_has_input_device("alsa") {
        if system {
            warnings.push(
                "System audio needs an FFmpeg build with PulseAudio support; it was skipped."
                    .into(),
            );
        }
        if microphone {
            inputs.push(
                ["-thread_queue_size", "1024", "-f", "alsa", "-i", "default"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            );
        }
    } else {
        warnings.push("This FFmpeg build cannot capture audio; recording video only.".into());
    }
    (inputs, warnings)
}

// ---------------------------------------------------------------------------
// Cameras
// ---------------------------------------------------------------------------

fn camera_devices() -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut devices: Vec<(u32, PathBuf, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let node = entry.file_name().to_string_lossy().to_string();
            let number: u32 = node.strip_prefix("video")?.parse().ok()?;
            let base = entry.path();
            // Each camera exposes extra metadata nodes; index 0 is the capture node.
            let index = std::fs::read_to_string(base.join("index"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "0".into());
            if index != "0" {
                return None;
            }
            let name = std::fs::read_to_string(base.join("name"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let name = if name.is_empty() {
                format!("Camera {}", number + 1)
            } else {
                name
            };
            Some((number, PathBuf::from(format!("/dev/{node}")), name))
        })
        .collect();
    devices.sort_by_key(|(n, _, _)| *n);
    devices.into_iter().map(|(_, p, n)| (p, n)).collect()
}

pub fn list_cameras() -> Vec<String> {
    camera_devices().into_iter().map(|(_, name)| name).collect()
}

pub fn camera_input(index: usize) -> Option<Vec<String>> {
    let (path, _) = camera_devices().into_iter().nth(index)?;
    Some(
        [
            "-thread_queue_size",
            "512",
            "-f",
            "v4l2",
            "-framerate",
            "30",
            "-i",
            &path.to_string_lossy(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    )
}

// ---------------------------------------------------------------------------
// Screenshots and previews
// ---------------------------------------------------------------------------

fn x11_area_for(target: &CaptureTarget, region: Option<Region>) -> Result<(i32, i32, u32, u32)> {
    match target {
        CaptureTarget::Display(display) => Ok(display_area(display, region)),
        CaptureTarget::Window(window) => window_area(window.id),
        CaptureTarget::Portal { .. } => bail!("not an X11 target"),
    }
}

fn grab_command(area: (i32, i32, u32, u32)) -> Result<Command> {
    let ffmpeg = ffmpeg::locate().ok_or_else(|| anyhow!("FFmpeg was not found"))?;
    let (x, y, width, height) = area;
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "x11grab",
            "-draw_mouse",
            "0",
            "-video_size",
        ])
        .arg(format!("{width}x{height}"))
        .arg("-i")
        .arg(format!("{}+{x},{y}", x_display()))
        .args(["-frames:v", "1"])
        .stdin(Stdio::null());
    Ok(command)
}

pub fn screenshot(target: &CaptureTarget, region: Option<Region>, output: &Path) -> Result<()> {
    if let CaptureTarget::Portal { .. } = target {
        return portal::screenshot(output);
    }
    let area = x11_area_for(target, region)?;
    let out = grab_command(area)?
        .args(["-update", "1"])
        .arg(output)
        .output()
        .context("failed to run ffmpeg")?;
    if !out.status.success() {
        bail!(
            "{}",
            explain_ffmpeg_error(String::from_utf8_lossy(&out.stderr).trim())
        );
    }
    Ok(())
}

pub fn grab(target: &CaptureTarget) -> Result<PreviewFrame> {
    let area = x11_area_for(target, None)?;
    let (_, _, width, height) = area;
    let out = grab_command(area)?
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .output()
        .context("failed to run ffmpeg")?;
    let expected = width as usize * height as usize * 4;
    if out.stdout.len() < expected {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut rgba = out.stdout;
    rgba.truncate(expected);
    Ok(PreviewFrame {
        width,
        height,
        rgba,
    })
}
