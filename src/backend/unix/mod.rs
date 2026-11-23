//! macOS and Linux backend.
//!
//! Recording is delegated to an `ffmpeg` child process:
//!
//! * Linux / X11  – `x11grab` video, PulseAudio/PipeWire audio, V4L2 webcam
//! * Linux / Wayland – xdg-desktop-portal ScreenCast → PipeWire → GStreamer
//!   (`pipewiresrc`) piped into ffmpeg
//! * macOS – `avfoundation` video, audio and webcam
//!
//! Pausing finishes the current segment; resuming starts a new one. When the
//! recording stops, all segments are joined losslessly into the final MP4.

mod ffmpeg;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod portal;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use parking_lot::Mutex;

use crate::media::{
    Capabilities, CaptureTarget, Codec, PauseClock, PreviewFrame, RecordingOptions, Region,
    WebcamOptions,
};

pub use self::platform::{ffmpeg_missing, list_cameras, list_displays, list_windows};

pub fn set_dpi_awareness() {}

/// The desktop draws the title bar on macOS and Linux.
pub fn style_title_bar(_title: &str, _rgb: [u8; 3]) {}

/// Not supported by the macOS/Linux capture paths; the bar is not shown there.
#[allow(dead_code)]
pub fn exclude_from_capture(_title: &str) -> bool {
    true
}

pub fn capabilities() -> Capabilities {
    let ffmpeg = ffmpeg::locate();
    let encoders = ffmpeg.as_deref().map(ffmpeg::encoders).unwrap_or_default();
    let h264 = ffmpeg::pick_encoder(Codec::H264, &encoders);
    let hevc = ffmpeg::pick_encoder(Codec::Hevc, &encoders);

    let mut caps = platform::base_capabilities();
    caps.hevc = hevc.is_some();
    caps.encoder_label = match h264.or(hevc) {
        Some(name) => ffmpeg::encoder_label(name).to_string(),
        None => "FFmpeg".to_string(),
    };

    if ffmpeg.is_none() {
        caps.missing.insert(0, platform::ffmpeg_missing());
    } else if h264.is_none() && hevc.is_none() {
        caps.missing.insert(
            0,
            crate::media::MissingDependency {
                title: "No supported video encoder".into(),
                detail: "The installed FFmpeg has no H.264 or HEVC encoder (libx264, libx265, \
                         libopenh264 or VideoToolbox). Install a full FFmpeg build."
                    .into(),
                install_command: platform::ffmpeg_missing().install_command,
            },
        );
    }
    caps
}

// ---------------------------------------------------------------------------
// Capture plan
// ---------------------------------------------------------------------------

/// Where the video frames come from.
#[derive(Clone, Debug)]
pub(crate) enum VideoInput {
    /// An X11 screen area, in root window coordinates.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    X11Area {
        display: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// A specific X11 window (ffmpeg >= 5 `-window_id`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    X11Window { display: String, window_id: u64 },
    /// An AVFoundation video device index ("Capture screen N").
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    AvFoundation { device_index: usize },
    /// A PipeWire node obtained from the ScreenCast portal (Wayland).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    PipeWire { node_id: u32 },
}

pub(crate) struct Plan {
    ffmpeg: PathBuf,
    video: VideoInput,
    crop: Option<Region>,
    /// Approximate output height, used to size the webcam overlay.
    out_height: u32,
    fps: u32,
    bitrate: u32,
    codec: Codec,
    encoder: &'static str,
    capture_cursor: bool,
    audio: Vec<Vec<String>>,
    webcam: Option<(Vec<String>, WebcamOptions)>,
}

impl Plan {
    fn segment_command(&self, output: &Path) -> Command {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "error".into(),
            "-y".into(),
        ];

        let fps = self.fps.to_string();
        match &self.video {
            VideoInput::X11Area {
                display,
                x,
                y,
                width,
                height,
            } => {
                args.extend(strings(&[
                    "-thread_queue_size",
                    "1024",
                    "-f",
                    "x11grab",
                    "-framerate",
                    &fps,
                    "-draw_mouse",
                    if self.capture_cursor { "1" } else { "0" },
                    "-video_size",
                    &format!("{width}x{height}"),
                    "-i",
                    &format!("{display}+{x},{y}"),
                ]));
            }
            VideoInput::X11Window { display, window_id } => {
                args.extend(strings(&[
                    "-thread_queue_size",
                    "1024",
                    "-f",
                    "x11grab",
                    "-framerate",
                    &fps,
                    "-draw_mouse",
                    if self.capture_cursor { "1" } else { "0" },
                    "-window_id",
                    &format!("0x{window_id:x}"),
                    "-i",
                    display,
                ]));
            }
            VideoInput::AvFoundation { device_index } => {
                args.extend(strings(&[
                    "-thread_queue_size",
                    "1024",
                    "-f",
                    "avfoundation",
                    "-capture_cursor",
                    if self.capture_cursor { "1" } else { "0" },
                    "-framerate",
                    &fps,
                    "-i",
                    &format!("{device_index}:none"),
                ]));
            }
            VideoInput::PipeWire { .. } => {
                args.extend(strings(&[
                    "-thread_queue_size",
                    "256",
                    "-f",
                    "yuv4mpegpipe",
                    "-i",
                    "pipe:0",
                ]));
            }
        }

        let mut next_input = 1usize;
        let mut audio_labels = Vec::new();
        for input in &self.audio {
            args.extend(input.iter().cloned());
            audio_labels.push(format!("[{next_input}:a]"));
            next_input += 1;
        }

        let webcam_input = self.webcam.as_ref().map(|(input, options)| {
            args.extend(input.iter().cloned());
            let index = next_input;
            next_input += 1;
            (index, options)
        });

        // ---- filter graph ------------------------------------------------
        let mut base = String::from("[0:v]");
        if let Some(crop) = self.crop {
            base.push_str(&format!(
                "crop={}:{}:{}:{},",
                crop.width, crop.height, crop.x, crop.y
            ));
        }
        // Encoders need even dimensions.
        base.push_str("crop=trunc(iw/2)*2:trunc(ih/2)*2");

        let mut graph = Vec::new();
        match webcam_input {
            Some((index, options)) => {
                let margin = ((self.out_height as f32 * 0.02).round() as u32).max(8);
                let cam_h =
                    ((self.out_height as f32 * options.size_pct.clamp(8, 60) as f32 / 100.0)
                        as u32
                        / 2
                        * 2)
                    .max(16);
                let mirror = if options.mirror { ",hflip" } else { "" };
                let (x, y) = match options.corner {
                    crate::media::Corner::TopLeft => (format!("{margin}"), format!("{margin}")),
                    crate::media::Corner::TopRight => {
                        (format!("main_w-overlay_w-{margin}"), format!("{margin}"))
                    }
                    crate::media::Corner::BottomLeft => {
                        (format!("{margin}"), format!("main_h-overlay_h-{margin}"))
                    }
                    crate::media::Corner::BottomRight => (
                        format!("main_w-overlay_w-{margin}"),
                        format!("main_h-overlay_h-{margin}"),
                    ),
                };
                graph.push(format!("{base}[base]"));
                graph.push(format!("[{index}:v]scale=-2:{cam_h}{mirror}[cam]"));
                graph.push(format!(
                    "[base][cam]overlay=x={x}:y={y}:eof_action=pass,format=yuv420p[vout]"
                ));
            }
            None => graph.push(format!("{base},format=yuv420p[vout]")),
        }

        match audio_labels.len() {
            0 => {}
            1 => graph.push(format!("{}aresample=async=1000[aout]", audio_labels[0])),
            n => graph.push(format!(
                "{}amix=inputs={n}:duration=longest:dropout_transition=0:normalize=0,aresample=async=1000[aout]",
                audio_labels.join("")
            )),
        }

        args.push("-filter_complex".into());
        args.push(graph.join(";"));
        args.extend(strings(&["-map", "[vout]"]));
        if !audio_labels.is_empty() {
            args.extend(strings(&["-map", "[aout]"]));
        }

        args.extend(ffmpeg::encoder_args(self.encoder, self.bitrate, self.fps));
        if !audio_labels.is_empty() {
            args.extend(strings(&[
                "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2",
            ]));
        }
        args.extend(strings(&[
            "-progress",
            "pipe:1",
            "-nostats",
            "-f",
            "matroska",
        ]));
        args.push(output.to_string_lossy().to_string());

        let mut command = Command::new(&self.ffmpeg);
        command.args(&args);
        command
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Segments
// ---------------------------------------------------------------------------

struct Segment {
    ffmpeg: Child,
    producer: Option<Child>,
    path: PathBuf,
    log: Arc<Mutex<VecDeque<String>>>,
    readers: Vec<JoinHandle<()>>,
}

impl Segment {
    fn spawn(
        plan: &Plan,
        path: PathBuf,
        session: Option<&SessionHandle>,
    ) -> Result<(Self, Arc<AtomicU64>)> {
        let mut command = plan.segment_command(&path);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut producer: Option<Child> = None;
        match (&plan.video, session) {
            #[cfg(target_os = "linux")]
            (VideoInput::PipeWire { node_id }, Some(SessionHandle::Portal(portal))) => {
                let mut child = portal::spawn_producer(portal, *node_id, plan.fps)?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| anyhow!("the PipeWire capture produced no output"))?;
                command.stdin(Stdio::from(stdout));
                producer = Some(child);
            }
            (VideoInput::PipeWire { .. }, _) => bail!("screen sharing session is not available"),
            _ => {
                command.stdin(Stdio::null());
            }
        }

        process_setup(&mut command);
        let mut ffmpeg = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                if let Some(mut producer) = producer {
                    let _ = producer.kill();
                    let _ = producer.wait();
                }
                return Err(anyhow!("failed to start ffmpeg: {err}"));
            }
        };

        let frames = Arc::new(AtomicU64::new(0));
        let log = Arc::new(Mutex::new(VecDeque::new()));
        let mut readers = Vec::new();

        if let Some(stdout) = ffmpeg.stdout.take() {
            let frames = frames.clone();
            readers.push(std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(value) = line.strip_prefix("frame=") {
                        if let Ok(count) = value.trim().parse::<u64>() {
                            frames.store(count, Ordering::Relaxed);
                        }
                    }
                }
            }));
        }
        if let Some(stderr) = ffmpeg.stderr.take() {
            readers.push(spawn_log_reader(stderr, log.clone()));
        }
        if let Some(stderr) = producer.as_mut().and_then(|p| p.stderr.take()) {
            readers.push(spawn_log_reader(stderr, log.clone()));
        }

        Ok((
            Self {
                ffmpeg,
                producer,
                path,
                log,
                readers,
            },
            frames,
        ))
    }

    fn is_running(&mut self) -> bool {
        matches!(self.ffmpeg.try_wait(), Ok(None))
    }

    fn log_tail(&self) -> String {
        let log = self.log.lock();
        let lines: Vec<&str> = log
            .iter()
            .map(|s| s.as_str())
            .filter(|line| !line.trim().is_empty())
            .collect();
        let start = lines.len().saturating_sub(4);
        lines[start..].join("\n")
    }

    /// Asks ffmpeg to finish the file and waits for it.
    fn finish(mut self) -> Result<PathBuf> {
        if self.is_running() {
            interrupt(&self.ffmpeg);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.ffmpeg.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = self.ffmpeg.kill();
                    let _ = self.ffmpeg.wait();
                    break;
                }
            }
        }
        if let Some(mut producer) = self.producer.take() {
            interrupt(&producer);
            std::thread::sleep(Duration::from_millis(50));
            let _ = producer.kill();
            let _ = producer.wait();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }

        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size < 1024 {
            let tail = self.log_tail();
            let _ = std::fs::remove_file(&self.path);
            if tail.is_empty() {
                bail!("ffmpeg did not record any video");
            }
            bail!("{tail}");
        }
        Ok(self.path.clone())
    }
}

fn spawn_log_reader<R: std::io::Read + Send + 'static>(
    stream: R,
    log: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let mut log = log.lock();
            log.push_back(line);
            if log.len() > 40 {
                log.pop_front();
            }
        }
    })
}

/// Graceful stop: ffmpeg finalizes the file on SIGINT.
fn interrupt(child: &Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
}

/// Makes sure helper processes do not outlive ScreenRec.
pub(crate) fn process_setup(command: &mut Command) {
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Backend resources that live as long as the recording.
pub(crate) enum SessionHandle {
    #[cfg(target_os = "linux")]
    Portal(portal::Portal),
}

// ---------------------------------------------------------------------------
// Active recording
// ---------------------------------------------------------------------------

struct State {
    current: Option<Segment>,
    segment_paths: Vec<PathBuf>,
    frame_counters: Vec<Arc<AtomicU64>>,
    finishing: Vec<JoinHandle<Result<PathBuf>>>,
    paused: bool,
    died_reported: bool,
}

pub struct ActiveRecording {
    plan: Plan,
    session: Option<SessionHandle>,
    work_dir: PathBuf,
    output: PathBuf,
    clock: PauseClock,
    state: Mutex<State>,
    error: Mutex<Option<String>>,
    finalized: bool,
}

impl ActiveRecording {
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed()
    }

    pub fn frames(&self) -> u64 {
        self.state
            .lock()
            .frame_counters
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    pub fn file_size(&self) -> u64 {
        self.state
            .lock()
            .segment_paths
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum()
    }

    pub fn is_capture_alive(&self) -> bool {
        let mut state = self.state.lock();
        if state.paused {
            return true;
        }
        let Some(current) = state.current.as_mut() else {
            return false;
        };
        if current.is_running() {
            return true;
        }
        if !state.died_reported {
            state.died_reported = true;
            let tail = state
                .current
                .as_ref()
                .map(|c| c.log_tail())
                .unwrap_or_default();
            let message = if tail.is_empty() {
                "The capture stopped unexpectedly.".to_string()
            } else {
                format!("The capture stopped unexpectedly: {tail}")
            };
            *self.error.lock() = Some(message);
        }
        false
    }

    pub fn pause(&self) {
        let mut state = self.state.lock();
        if state.paused {
            return;
        }
        state.paused = true;
        self.clock.pause();
        if let Some(segment) = state.current.take() {
            state
                .finishing
                .push(std::thread::spawn(move || segment.finish()));
        }
    }

    pub fn resume(&self) {
        let mut state = self.state.lock();
        if !state.paused {
            return;
        }
        let path = self
            .work_dir
            .join(format!("part{:03}.mkv", state.segment_paths.len()));
        match Segment::spawn(&self.plan, path.clone(), self.session.as_ref()) {
            Ok((segment, frames)) => {
                state.segment_paths.push(path);
                state.frame_counters.push(frames);
                state.current = Some(segment);
                state.paused = false;
                state.died_reported = false;
                self.clock.resume();
            }
            Err(err) => {
                *self.error.lock() = Some(format!("Could not resume: {err:#}"));
            }
        }
    }

    pub fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    /// Live audio levels are not available from the ffmpeg backend.
    pub fn audio_level(&self) -> Option<u32> {
        None
    }

    pub fn stop(mut self) -> Result<PathBuf> {
        self.finalize()?;
        Ok(self.output.clone())
    }

    fn finalize(&mut self) -> Result<()> {
        if self.finalized {
            return Ok(());
        }
        self.finalized = true;

        let (current, finishing) = {
            let mut state = self.state.lock();
            (state.current.take(), std::mem::take(&mut state.finishing))
        };

        let mut parts = Vec::new();
        let mut last_error = None;
        for handle in finishing {
            match handle.join() {
                Ok(Ok(path)) => parts.push(path),
                Ok(Err(err)) => last_error = Some(err),
                Err(_) => last_error = Some(anyhow!("segment finalizer panicked")),
            }
        }
        if let Some(segment) = current {
            match segment.finish() {
                Ok(path) => parts.push(path),
                Err(err) => last_error = Some(err),
            }
        }
        parts.sort();

        // Ends the screen sharing session (Linux portal), if any.
        self.session = None;

        if parts.is_empty() {
            let _ = std::fs::remove_dir_all(&self.work_dir);
            return Err(last_error.unwrap_or_else(|| anyhow!("nothing was recorded")));
        }

        let result =
            ffmpeg::join_segments(&self.plan.ffmpeg, &parts, &self.output, self.plan.codec);
        if result.is_ok() {
            let _ = std::fs::remove_dir_all(&self.work_dir);
        }
        result.with_context(|| {
            format!(
                "could not write the final video; the raw parts were kept in {}",
                self.work_dir.display()
            )
        })
    }
}

impl Drop for ActiveRecording {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

pub fn start(options: RecordingOptions) -> Result<ActiveRecording> {
    let ffmpeg = ffmpeg::locate().ok_or_else(|| {
        anyhow!(
            "FFmpeg was not found. {}",
            platform::ffmpeg_missing().detail
        )
    })?;
    let encoders = ffmpeg::encoders(&ffmpeg);
    let encoder = ffmpeg::pick_encoder(options.codec, &encoders).ok_or_else(|| {
        anyhow!(
            "this FFmpeg build has no {} encoder; choose another codec or install a full FFmpeg build",
            options.codec.label()
        )
    })?;

    if let Some(parent) = options.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let source = platform::prepare_video(&options)?;
    let mut warnings = source.warnings;

    let (audio, audio_warnings) = platform::audio_inputs(options.system_audio, options.microphone);
    warnings.extend(audio_warnings);

    let webcam = match &options.webcam {
        Some(cam) => match platform::camera_input(cam.device_index) {
            Some(input) => Some((input, cam.clone())),
            None => {
                warnings.push("Webcam unavailable: the selected camera was not found.".into());
                None
            }
        },
        None => None,
    };

    let plan = Plan {
        ffmpeg,
        video: source.input,
        crop: source.crop,
        out_height: source.out_height.max(2),
        fps: options.fps.clamp(1, 240),
        bitrate: options.bitrate.max(500_000),
        codec: options.codec,
        encoder,
        capture_cursor: options.capture_cursor,
        audio,
        webcam,
    };

    let stem = options
        .output
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "recording".into());
    let work_dir = options
        .output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{stem}.parts"));
    std::fs::create_dir_all(&work_dir)
        .with_context(|| format!("failed to create {}", work_dir.display()))?;

    let first = work_dir.join("part000.mkv");
    let (mut segment, frames) = match Segment::spawn(&plan, first.clone(), source.session.as_ref())
    {
        Ok(started) => started,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&work_dir);
            return Err(err);
        }
    };

    // Catch immediate failures (bad device, permission denied, ...).
    let probe_until = Instant::now() + Duration::from_millis(900);
    while Instant::now() < probe_until {
        if !segment.is_running() {
            let tail = segment.log_tail();
            let _ = segment.finish();
            let _ = std::fs::remove_dir_all(&work_dir);
            if tail.is_empty() {
                bail!("ffmpeg exited immediately");
            }
            bail!("{}", platform::explain_ffmpeg_error(&tail));
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(ActiveRecording {
        plan,
        session: source.session,
        work_dir,
        output: options.output,
        clock: PauseClock::new(),
        state: Mutex::new(State {
            current: Some(segment),
            segment_paths: vec![first],
            frame_counters: vec![frames],
            finishing: Vec::new(),
            paused: false,
            died_reported: false,
        }),
        error: Mutex::new((!warnings.is_empty()).then(|| warnings.join(" "))),
        finalized: false,
    })
}

/// The video source chosen for a recording.
pub(crate) struct PreparedVideo {
    pub input: VideoInput,
    pub crop: Option<Region>,
    pub out_height: u32,
    pub session: Option<SessionHandle>,
    pub warnings: Vec<String>,
}

pub fn screenshot(
    target: &CaptureTarget,
    region: Option<Region>,
    output: PathBuf,
) -> Result<PathBuf> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }
    platform::screenshot(target, region, &output)?;
    if !output.exists() {
        bail!("the screenshot could not be saved");
    }
    Ok(output)
}

pub fn grab(target: &CaptureTarget) -> Result<PreviewFrame> {
    platform::grab(target)
}

/// Converts an image file to RGBA pixels with ffmpeg, scaled to `width`×`height`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn decode_image(path: &Path, width: u32, height: u32) -> Result<PreviewFrame> {
    let ffmpeg = ffmpeg::locate().ok_or_else(|| anyhow!("FFmpeg was not found"))?;
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={width}:{height}"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .output()
        .context("failed to run ffmpeg")?;
    let expected = width as usize * height as usize * 4;
    if output.stdout.len() < expected {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let mut rgba = output.stdout;
    rgba.truncate(expected);
    Ok(PreviewFrame {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Corner;

    fn plan(audio: usize, webcam: bool) -> Plan {
        Plan {
            ffmpeg: PathBuf::from("ffmpeg"),
            video: VideoInput::X11Area {
                display: ":0".into(),
                x: 10,
                y: 20,
                width: 1280,
                height: 720,
            },
            crop: None,
            out_height: 720,
            fps: 30,
            bitrate: 8_000_000,
            codec: Codec::H264,
            encoder: "libx264",
            capture_cursor: true,
            audio: (0..audio)
                .map(|_| strings(&["-f", "pulse", "-i", "default"]))
                .collect(),
            webcam: webcam.then(|| {
                (
                    strings(&["-f", "v4l2", "-i", "/dev/video0"]),
                    WebcamOptions {
                        corner: Corner::TopRight,
                        ..Default::default()
                    },
                )
            }),
        }
    }

    fn args(plan: &Plan) -> Vec<String> {
        plan.segment_command(Path::new("out.mkv"))
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn video_only_graph() {
        let args = args(&plan(0, false));
        assert!(args.contains(&":0+10,20".to_string()));
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.ends_with("format=yuv420p[vout]"), "{graph}");
        assert!(!args.contains(&"[aout]".to_string()));
        assert_eq!(args.last().unwrap(), "out.mkv");
    }

    #[test]
    fn mixes_two_audio_inputs_and_overlays_webcam() {
        let args = args(&plan(2, true));
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[1:a][2:a]amix=inputs=2"), "{graph}");
        assert!(graph.contains("[3:v]scale=-2:158,hflip[cam]"), "{graph}");
        assert!(graph.contains("x=main_w-overlay_w-14:y=14"), "{graph}");
        assert!(args.contains(&"[aout]".to_string()));
    }
}
