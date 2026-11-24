//! Rendering the timeline to an MP4 with FFmpeg.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _, Result};

use crate::media::Codec;
use crate::tools;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportQuality {
    High,
    Balanced,
    Small,
}

impl ExportQuality {
    pub const ALL: [ExportQuality; 3] = [
        ExportQuality::High,
        ExportQuality::Balanced,
        ExportQuality::Small,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExportQuality::High => "High",
            ExportQuality::Balanced => "Balanced",
            ExportQuality::Small => "Small file",
        }
    }

    fn crf(self) -> u8 {
        match self {
            ExportQuality::High => 18,
            ExportQuality::Balanced => 22,
            ExportQuality::Small => 27,
        }
    }

    fn bpp(self) -> f64 {
        match self {
            ExportQuality::High => 0.20,
            ExportQuality::Balanced => 0.12,
            ExportQuality::Small => 0.07,
        }
    }
}

/// One timeline clip, resolved for rendering.
pub struct ExportClip {
    pub source: usize,
    pub start: f64,
    pub end: f64,
    pub volume: f32,
    pub has_audio: bool,
}

pub struct ExportJob {
    pub sources: Vec<PathBuf>,
    pub clips: Vec<ExportClip>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub quality: ExportQuality,
    pub output: PathBuf,
}

impl ExportJob {
    pub fn duration(&self) -> f64 {
        self.clips.iter().map(|c| c.end - c.start).sum()
    }

    fn filter_graph(&self) -> (String, bool) {
        let with_audio = self.clips.iter().any(|c| c.has_audio);
        let (w, h, fps) = (self.width, self.height, self.fps);
        let mut parts = Vec::new();
        let mut concat_inputs = String::new();

        for (k, clip) in self.clips.iter().enumerate() {
            parts.push(format!(
                "[{s}:v]trim=start={a:.3}:end={b:.3},setpts=PTS-STARTPTS,\
                 scale={w}:{h}:force_original_aspect_ratio=decrease,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps},format=yuv420p[v{k}]",
                s = clip.source,
                a = clip.start,
                b = clip.end,
            ));
            concat_inputs.push_str(&format!("[v{k}]"));
            if with_audio {
                if clip.has_audio {
                    parts.push(format!(
                        "[{s}:a]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS,\
                         volume={vol:.3},aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo[a{k}]",
                        s = clip.source,
                        a = clip.start,
                        b = clip.end,
                        vol = clip.volume,
                    ));
                } else {
                    parts.push(format!(
                        "anullsrc=r=48000:cl=stereo,atrim=duration={d:.3},\
                         aformat=sample_fmts=fltp:channel_layouts=stereo[a{k}]",
                        d = clip.end - clip.start,
                    ));
                }
                concat_inputs.push_str(&format!("[a{k}]"));
            }
        }
        parts.push(format!(
            "{concat_inputs}concat=n={}:v=1:a={}[vout]{}",
            self.clips.len(),
            u8::from(with_audio),
            if with_audio { "[aout]" } else { "" }
        ));
        (parts.join(";"), with_audio)
    }

    pub fn args(&self, encoder: &str) -> Vec<String> {
        let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for source in &self.sources {
            args.push("-i".into());
            args.push(source.to_string_lossy().to_string());
        }
        let (graph, with_audio) = self.filter_graph();
        args.extend([
            "-filter_complex".into(),
            graph,
            "-map".into(),
            "[vout]".into(),
        ]);
        if with_audio {
            args.extend(["-map".into(), "[aout]".into()]);
        }
        let bitrate =
            (self.width as f64 * self.height as f64 * self.fps as f64 * self.quality.bpp())
                .clamp(1_500_000.0, 80_000_000.0) as u32;
        args.extend(tools::encoder_args(
            encoder,
            bitrate,
            self.fps,
            Some(self.quality.crf()),
        ));
        args.extend(["-pix_fmt".into(), "yuv420p".into()]);
        if with_audio {
            args.extend(
                ["-c:a", "aac", "-b:a", "192k"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
        args.extend(
            ["-movflags", "+faststart", "-progress", "pipe:1", "-nostats"]
                .iter()
                .map(|s| s.to_string()),
        );
        args.push(self.output.to_string_lossy().to_string());
        args
    }
}

pub struct ExportHandle {
    pub progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    pub done: Receiver<Result<PathBuf, String>>,
    pub output: PathBuf,
}

impl ExportHandle {
    /// Progress in 0..=1.
    pub fn fraction(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

pub fn start(job: ExportJob, ctx: egui::Context) -> Result<ExportHandle> {
    if job.clips.is_empty() {
        bail!("the timeline is empty");
    }
    let ffmpeg = tools::ffmpeg().ok_or_else(|| anyhow!("FFmpeg was not found"))?;
    let encoder = tools::pick_encoder(Codec::H264, &tools::encoders(&ffmpeg))
        .ok_or_else(|| anyhow!("this FFmpeg build has no H.264 encoder"))?;
    if let Some(dir) = job.output.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }

    let progress = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, done) = mpsc::channel();
    let output = job.output.clone();
    let total = job.duration().max(0.01);
    let args = job.args(encoder);

    let mut child = tools::command(&ffmpeg)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg")?;

    let thread_progress = progress.clone();
    let thread_cancel = cancel.clone();
    let thread_output = output.clone();
    std::thread::Builder::new()
        .name("scroder-export".into())
        .spawn(move || {
            let stderr = child.stderr.take();
            let errors = std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(stderr) = stderr {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        text.push_str(&line);
                        text.push('\n');
                    }
                }
                text
            });

            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if thread_cancel.load(Ordering::Relaxed) {
                        let _ = child.kill();
                        break;
                    }
                    if let Some(us) = line.strip_prefix("out_time_us=") {
                        if let Ok(us) = us.trim().parse::<f64>() {
                            let fraction = (us / 1_000_000.0 / total).clamp(0.0, 1.0);
                            thread_progress.store((fraction * 1000.0) as u32, Ordering::Relaxed);
                            ctx.request_repaint();
                        }
                    }
                }
            }
            let status = child.wait();
            let errors = errors.join().unwrap_or_default();
            let result = if thread_cancel.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&thread_output);
                Err("Export cancelled.".to_string())
            } else {
                match status {
                    Ok(status) if status.success() && thread_output.exists() => {
                        thread_progress.store(1000, Ordering::Relaxed);
                        Ok(thread_output)
                    }
                    _ => {
                        let tail: Vec<&str> = errors.lines().rev().take(3).collect();
                        let tail: Vec<&str> = tail.into_iter().rev().collect();
                        Err(format!("Export failed: {}", tail.join(" ").trim()))
                    }
                }
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        })
        .context("failed to start the export thread")?;

    Ok(ExportHandle {
        progress,
        cancel,
        done,
        output,
    })
}

pub fn default_output(first_source: &Path) -> PathBuf {
    let stem = first_source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".into());
    crate::util::recordings_dir().join(format!("{stem}_edited.mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_concats_clips_and_fills_missing_audio() {
        let job = ExportJob {
            sources: vec!["a.mp4".into(), "b.mp4".into()],
            clips: vec![
                ExportClip {
                    source: 0,
                    start: 1.0,
                    end: 3.5,
                    volume: 1.0,
                    has_audio: true,
                },
                ExportClip {
                    source: 1,
                    start: 0.0,
                    end: 2.0,
                    volume: 0.5,
                    has_audio: false,
                },
            ],
            width: 1280,
            height: 720,
            fps: 30,
            quality: ExportQuality::Balanced,
            output: "out.mp4".into(),
        };
        let (graph, audio) = job.filter_graph();
        assert!(audio);
        assert!(graph.contains("[0:v]trim=start=1.000:end=3.500"));
        assert!(graph.contains("anullsrc=r=48000:cl=stereo,atrim=duration=2.000"));
        assert!(graph.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]"));
        assert!((job.duration() - 4.5).abs() < 1e-9);
        let args = job.args("libx264");
        assert!(args.windows(2).any(|w| w[0] == "-crf" && w[1] == "22"));
    }

    /// Renders a real project. Needs FFmpeg (or `SCRODER_FFMPEG`) and
    /// `SCRODER_TEST_VIDEOS` = two video paths separated by `;`.
    #[test]
    #[ignore = "needs ffmpeg and sample videos"]
    fn renders_real_export() {
        let videos = std::env::var("SCRODER_TEST_VIDEOS").expect("SCRODER_TEST_VIDEOS");
        let sources: Vec<PathBuf> = videos.split(';').map(PathBuf::from).collect();
        let infos: Vec<_> = sources
            .iter()
            .map(|s| super::super::media::probe(s).unwrap())
            .collect();
        let output = std::env::temp_dir().join("scroder-export-test.mp4");
        let job = ExportJob {
            clips: vec![
                ExportClip {
                    source: 0,
                    start: 1.0,
                    end: 3.0,
                    volume: 1.0,
                    has_audio: infos[0].has_audio,
                },
                ExportClip {
                    source: 1,
                    start: 0.0,
                    end: 1.5,
                    volume: 1.0,
                    has_audio: infos[1].has_audio,
                },
                ExportClip {
                    source: 0,
                    start: 8.0,
                    end: 9.0,
                    volume: 0.0,
                    has_audio: infos[0].has_audio,
                },
            ],
            sources,
            width: 1280,
            height: 720,
            fps: 30,
            quality: ExportQuality::Balanced,
            output: output.clone(),
        };
        let handle = start(job, egui::Context::default()).unwrap();
        let result = handle.done.recv().unwrap();
        assert_eq!(result.as_deref().ok(), Some(output.as_path()), "{result:?}");
        let info = super::super::media::probe(&output).unwrap();
        assert!(
            (info.duration - 4.5).abs() < 0.2,
            "duration {}",
            info.duration
        );
        assert_eq!((info.width, info.height), (1280, 720));
        assert!(info.has_audio);
    }
}
