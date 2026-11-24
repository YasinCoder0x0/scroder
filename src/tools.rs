//! Locating and running FFmpeg (ffmpeg, ffprobe, ffplay) on every platform.
//!
//! The Linux/macOS recorder and the built-in video editor both use FFmpeg.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::media::Codec;

const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// A `Command` that never flashes a console window on Windows.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.stdin(Stdio::null());
    command
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Finds an executable: next to Scroder (bundled), on `PATH`, then in the
/// usual package manager locations. GUI apps on macOS do not inherit the
/// shell's `PATH` and Windows apps keep the `PATH` they started with, so
/// common install folders are probed explicitly.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{EXE}");
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            dirs.push(dir.join("ffmpeg"));
            dirs.push(dir.join("../Resources"));
            dirs.push(dir.join("../lib/scroder"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }

    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
            // `winget install Gyan.FFmpeg` unpacks into a versioned folder.
            if let Ok(entries) =
                std::fs::read_dir(local.join("Microsoft").join("WinGet").join("Packages"))
            {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().contains("FFmpeg") {
                        if let Ok(inner) = std::fs::read_dir(entry.path()) {
                            dirs.extend(inner.flatten().map(|e| e.path().join("bin")));
                        }
                    }
                }
            }
        }
        if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            dirs.push(home.join("scoop").join("shims"));
        }
        for dir in [
            r"C:\ffmpeg\bin",
            r"C:\Program Files\ffmpeg\bin",
            r"C:\ProgramData\chocolatey\bin",
        ] {
            dirs.push(PathBuf::from(dir));
        }
    }
    #[cfg(unix)]
    for dir in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/opt/local/bin",
        "/usr/bin",
        "/bin",
        "/snap/bin",
    ] {
        dirs.push(PathBuf::from(dir));
    }

    dirs.into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| is_executable(path))
}

pub fn ffmpeg() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("SCRODER_FFMPEG") {
        let custom = PathBuf::from(custom);
        if is_executable(&custom) {
            return Some(custom);
        }
    }
    find_tool("ffmpeg")
}

/// ffprobe / ffplay, preferring the copy next to the ffmpeg in use.
fn sibling(name: &str) -> Option<PathBuf> {
    ffmpeg()
        .and_then(|ffmpeg| ffmpeg.parent().map(|dir| dir.join(format!("{name}{EXE}"))))
        .filter(|path| is_executable(path))
        .or_else(|| find_tool(name))
}

pub fn ffprobe() -> Option<PathBuf> {
    sibling("ffprobe")
}

pub fn ffplay() -> Option<PathBuf> {
    sibling("ffplay")
}

fn encoder_cache() -> &'static Mutex<HashMap<PathBuf, Vec<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Names of the video encoders compiled into this ffmpeg.
pub fn encoders(ffmpeg: &Path) -> Vec<String> {
    if let Some(cached) = encoder_cache().lock().get(ffmpeg) {
        return cached.clone();
    }
    let list: Vec<String> = command(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split_whitespace();
                    let flags = parts.next()?;
                    let name = parts.next()?;
                    (flags.len() == 6 && flags.starts_with('V')).then(|| name.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    if !list.is_empty() {
        encoder_cache()
            .lock()
            .insert(ffmpeg.to_path_buf(), list.clone());
    }
    list
}

/// Best available encoder for `codec`, preferring hardware where it is
/// reliable and x264/x265 otherwise.
pub fn pick_encoder(codec: Codec, available: &[String]) -> Option<&'static str> {
    let preferred: &[&'static str] = match codec {
        Codec::H264 if cfg!(target_os = "macos") => {
            &["h264_videotoolbox", "libx264", "libopenh264"]
        }
        Codec::H264 if cfg!(windows) => {
            &["libx264", "h264_mf", "h264_nvenc", "h264_qsv", "h264_amf"]
        }
        Codec::H264 => &["libx264", "libopenh264"],
        Codec::Hevc if cfg!(target_os = "macos") => &["hevc_videotoolbox", "libx265"],
        Codec::Hevc if cfg!(windows) => &["libx265", "hevc_mf", "hevc_nvenc", "hevc_qsv"],
        Codec::Hevc => &["libx265"],
    };
    preferred
        .iter()
        .copied()
        .find(|name| available.iter().any(|a| a == name))
}

#[cfg_attr(windows, allow(dead_code))]
pub fn encoder_label(encoder: &str) -> &'static str {
    match encoder {
        "h264_videotoolbox" | "hevc_videotoolbox" => "Hardware accelerated (VideoToolbox)",
        "h264_mf" | "hevc_mf" => "Hardware accelerated (Media Foundation)",
        "h264_nvenc" | "hevc_nvenc" => "Hardware accelerated (NVENC)",
        "h264_qsv" | "hevc_qsv" => "Hardware accelerated (Quick Sync)",
        "h264_amf" => "Hardware accelerated (AMF)",
        "libx264" => "Software encoder (x264)",
        "libx265" => "Software encoder (x265)",
        "libopenh264" => "Software encoder (OpenH264)",
        _ => "FFmpeg",
    }
}

/// Encoder arguments. `crf` (when given) selects constant quality for the
/// x264/x265 encoders, which gives the best size/quality for exports.
pub fn encoder_args(encoder: &str, bitrate: u32, fps: u32, crf: Option<u8>) -> Vec<String> {
    let rate = bitrate.to_string();
    let max = (bitrate as u64 * 3 / 2).to_string();
    let buf = (bitrate as u64 * 2).to_string();
    let gop = (fps.max(1) * 2).to_string();
    let crf = crf.map(|c| c.to_string());
    let mut args: Vec<&str> = vec!["-c:v", encoder];
    match (encoder, crf.as_deref()) {
        ("libx264", Some(crf)) => args.extend(["-preset", "medium", "-crf", crf, "-g", &gop]),
        ("libx265", Some(crf)) => args.extend([
            "-preset",
            "fast",
            "-crf",
            crf,
            "-x265-params",
            "log-level=error",
        ]),
        ("libx264", None) => args.extend([
            "-preset", "veryfast", "-b:v", &rate, "-maxrate", &max, "-bufsize", &buf, "-g", &gop,
        ]),
        ("libx265", None) => args.extend([
            "-preset",
            "ultrafast",
            "-b:v",
            &rate,
            "-g",
            &gop,
            "-x265-params",
            "log-level=error",
        ]),
        ("h264_videotoolbox" | "hevc_videotoolbox", _) => args.extend([
            "-b:v",
            &rate,
            "-maxrate",
            &max,
            "-bufsize",
            &buf,
            "-g",
            &gop,
            "-realtime",
            "1",
            "-allow_sw",
            "1",
        ]),
        _ => args.extend(["-b:v", &rate, "-g", &gop]),
    }
    args.into_iter().map(String::from).collect()
}
