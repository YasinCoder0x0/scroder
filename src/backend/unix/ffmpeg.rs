//! Locating ffmpeg, probing its encoders and joining recorded segments.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context as _, Result};

use crate::media::Codec;

#[cfg(target_os = "linux")]
pub use crate::tools::find_tool;
pub use crate::tools::{encoder_label, encoders, pick_encoder};

pub fn locate() -> Option<PathBuf> {
    crate::tools::ffmpeg()
}

pub fn encoder_args(encoder: &str, bitrate: u32, fps: u32) -> Vec<String> {
    crate::tools::encoder_args(encoder, bitrate, fps, None)
}

/// Whether this ffmpeg's x11grab supports `-window_id` (ffmpeg >= 5.0).
#[cfg(target_os = "linux")]
pub fn x11grab_supports_window_id(ffmpeg: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use parking_lot::Mutex;

    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(value) = cache.lock().get(ffmpeg) {
        return *value;
    }
    let supported = Command::new(ffmpeg)
        .args(["-hide_banner", "-h", "demuxer=x11grab"])
        .stdin(Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("window_id"))
        .unwrap_or(false);
    cache.lock().insert(ffmpeg.to_path_buf(), supported);
    supported
}

/// Losslessly joins the recorded segments into the final MP4.
pub fn join_segments(ffmpeg: &Path, parts: &[PathBuf], output: &Path, codec: Codec) -> Result<()> {
    let dir = parts[0].parent().unwrap_or_else(|| Path::new("."));
    let list_path = dir.join("parts.txt");
    let list: String = parts
        .iter()
        .map(|part| {
            let escaped = part.to_string_lossy().replace('\'', "'\\''");
            format!("file '{escaped}'\n")
        })
        .collect();
    std::fs::write(&list_path, list).context("failed to write the segment list")?;

    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg(&list_path)
        .args(["-c", "copy"]);
    if codec == Codec::Hevc {
        // QuickTime / Apple players need the hvc1 tag.
        command.args(["-tag:v", "hvc1"]);
    }
    command
        .args(["-movflags", "+faststart"])
        .arg(output)
        .stdin(Stdio::null());

    let out = command.output().context("failed to run ffmpeg")?;
    if !out.status.success() || !output.exists() {
        bail!(
            "joining the recording failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
