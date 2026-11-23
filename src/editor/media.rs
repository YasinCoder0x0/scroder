//! Reading media with FFmpeg: probing, thumbnail strips, waveforms and
//! single preview frames.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;

use anyhow::{anyhow, bail, Context as _, Result};
use egui::ColorImage;

use crate::tools;

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
}

fn parse_rate(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    (den > 0.0 && num > 0.0).then(|| num / den)
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let ffprobe = tools::ffprobe().ok_or_else(|| anyhow!("ffprobe was not found"))?;
    let out = tools::command(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,width,height,avg_frame_rate,r_frame_rate:format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .context("failed to run ffprobe")?;
    if !out.status.success() {
        bail!(
            "this file could not be read: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("unexpected ffprobe output")?;
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| anyhow!("this file has no video stream"))?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let duration = json["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    if duration <= 0.05 {
        bail!("this file has no playable duration");
    }
    let fps = video["avg_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .or_else(|| video["r_frame_rate"].as_str().and_then(parse_rate))
        .filter(|f| *f > 1.0 && *f < 500.0)
        .unwrap_or(30.0);
    Ok(MediaInfo {
        duration,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps,
        has_audio,
    })
}

/// Fits `width`×`height` inside a `max_w`×`max_h` box (even dimensions).
pub fn fit(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = (width.max(2) as f64, height.max(2) as f64);
    let scale = (max_w as f64 / w).min(max_h as f64 / h).min(1.0);
    let even = |v: f64| ((v.round() as u32) / 2 * 2).max(2);
    (even(w * scale), even(h * scale))
}

fn read_frames(
    mut child: std::process::Child,
    frame_len: usize,
    mut on_frame: impl FnMut(Vec<u8>) -> bool,
) -> Result<()> {
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no output"))?;
    loop {
        let mut frame = vec![0u8; frame_len];
        if stdout.read_exact(&mut frame).is_err() {
            break;
        }
        if !on_frame(frame) {
            let _ = child.kill();
            break;
        }
    }
    let _ = child.wait();
    Ok(())
}

/// Evenly spaced thumbnails: (time in seconds, image).
pub fn thumbnails(path: &Path, info: &MediaInfo, height: u32) -> Result<Vec<(f64, ColorImage)>> {
    let ffmpeg = tools::ffmpeg().ok_or_else(|| anyhow!("ffmpeg was not found"))?;
    let (w, h) = fit(info.width, info.height, height * 4, height);
    let count = (info.duration / 2.0).clamp(4.0, 60.0);
    let interval = (info.duration / count).max(0.5);

    let mut command = tools::command(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error"]);
    if info.duration > 120.0 {
        // Keyframes are plenty for a strip and decode far faster.
        command.args(["-skip_frame", "nokey"]);
    }
    let child = command
        .arg("-i")
        .arg(path)
        .args([
            "-an",
            "-vf",
            &format!("fps=1/{interval:.3},scale={w}:{h}"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run ffmpeg")?;

    let mut thumbs = Vec::new();
    read_frames(child, (w * h * 4) as usize, |frame| {
        let t = thumbs.len() as f64 * interval;
        thumbs.push((
            t,
            ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &frame),
        ));
        thumbs.len() < 80
    })?;
    Ok(thumbs)
}

/// Peak levels (0..1), 50 per second.
pub const WAVE_RATE: f64 = 50.0;

pub fn waveform(path: &Path) -> Result<Vec<f32>> {
    let ffmpeg = tools::ffmpeg().ok_or_else(|| anyhow!("ffmpeg was not found"))?;
    let mut child = tools::command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "2000", "-f", "s16le", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to run ffmpeg")?;
    let mut bytes = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_end(&mut bytes);
    }
    let _ = child.wait();

    let per_bucket = (2000.0 / WAVE_RATE) as usize;
    let mut peaks: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
        .collect::<Vec<u16>>()
        .chunks(per_bucket)
        .map(|chunk| *chunk.iter().max().unwrap_or(&0) as f32 / i16::MAX as f32)
        .collect();
    let max = peaks.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.01 {
        // Normalise so quiet recordings still show a readable shape.
        let gain = (0.95 / max).min(4.0);
        for peak in &mut peaks {
            *peak = (*peak * gain).min(1.0);
        }
    }
    Ok(peaks)
}

/// Decodes the frame at `time` scaled to `width`×`height`.
pub fn frame_at(path: &Path, time: f64, width: u32, height: u32) -> Result<ColorImage> {
    let ffmpeg = tools::ffmpeg().ok_or_else(|| anyhow!("ffmpeg was not found"))?;
    let out = tools::command(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{:.3}", time.max(0.0)),
            "-i",
        ])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-an",
            "-vf",
            &format!("scale={width}:{height}"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ])
        .output()
        .context("failed to run ffmpeg")?;
    let len = (width * height * 4) as usize;
    if out.stdout.len() < len {
        bail!("no frame at {time:.2}s");
    }
    Ok(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &out.stdout[..len],
    ))
}
