//! Preview playback.
//!
//! * `FrameWorker` decodes single frames for scrubbing (newest request wins).
//! * `VideoStream` decodes one clip continuously while playing.
//! * `AudioPreview` plays the clip's sound through `ffplay`, when available.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread::JoinHandle;

use egui::ColorImage;

use super::media;
use crate::tools;

pub const PREVIEW_FPS: f64 = 30.0;

pub struct FrameRequest {
    pub path: PathBuf,
    pub time: f64,
    pub size: (u32, u32),
    pub generation: u64,
}

pub struct FrameWorker {
    tx: Sender<FrameRequest>,
    rx: Receiver<(u64, ColorImage)>,
    _join: JoinHandle<()>,
}

impl FrameWorker {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, requests) = mpsc::channel::<FrameRequest>();
        let (done_tx, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("screenrec-preview".into())
            .spawn(move || {
                while let Ok(mut request) = requests.recv() {
                    // Skip to the newest request.
                    while let Ok(newer) = requests.try_recv() {
                        request = newer;
                    }
                    let (w, h) = request.size;
                    let image = media::frame_at(&request.path, request.time, w, h).or_else(|_| {
                        // Seeking to the very last frame can come back empty.
                        media::frame_at(&request.path, (request.time - 0.25).max(0.0), w, h)
                    });
                    if let Ok(image) = image {
                        if done_tx.send((request.generation, image)).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
            })
            .expect("failed to start the preview thread");
        Self {
            tx,
            rx,
            _join: join,
        }
    }

    pub fn request(&self, request: FrameRequest) {
        let _ = self.tx.send(request);
    }

    pub fn poll(&self) -> Option<(u64, ColorImage)> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        latest
    }
}

/// Continuous decoding of one clip, from `start` (source seconds) for
/// `duration` seconds, at `PREVIEW_FPS`.
pub struct VideoStream {
    child: Child,
    rx: Receiver<ColorImage>,
    pub start: f64,
    pub frames_taken: u64,
    pub ended: bool,
    reader: Option<JoinHandle<()>>,
}

impl VideoStream {
    pub fn start(
        path: &Path,
        start: f64,
        duration: f64,
        size: (u32, u32),
        ctx: egui::Context,
    ) -> Option<Self> {
        let ffmpeg = tools::ffmpeg()?;
        let (w, h) = size;
        let mut child = tools::command(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                &format!("{start:.3}"),
                "-i",
            ])
            .arg(path)
            .args([
                "-t",
                &format!("{:.3}", duration.max(0.05)),
                "-an",
                "-vf",
                &format!("fps={PREVIEW_FPS},scale={w}:{h}"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let (tx, rx): (SyncSender<ColorImage>, Receiver<ColorImage>) = mpsc::sync_channel(8);
        let reader = std::thread::spawn(move || {
            let len = (w * h * 4) as usize;
            loop {
                let mut frame = vec![0u8; len];
                if stdout.read_exact(&mut frame).is_err() {
                    break;
                }
                let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &frame);
                if tx.send(image).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Some(Self {
            child,
            rx,
            start,
            frames_taken: 0,
            ended: false,
            reader: Some(reader),
        })
    }

    /// Returns the newest frame due at `source_time`, if any.
    pub fn frame_for(&mut self, source_time: f64) -> Option<ColorImage> {
        let due = ((source_time - self.start) * PREVIEW_FPS).floor().max(0.0) as u64 + 1;
        let mut latest = None;
        while self.frames_taken < due {
            match self.rx.try_recv() {
                Ok(frame) => {
                    self.frames_taken += 1;
                    latest = Some(frame);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.ended = true;
                    break;
                }
            }
        }
        latest
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Unblock the reader if it is waiting on a full channel.
        while self.rx.try_recv().is_ok() {}
        if let Some(reader) = self.reader.take() {
            drop(std::mem::replace(&mut self.rx, mpsc::sync_channel(1).1));
            let _ = reader.join();
        }
    }
}

pub struct AudioPreview {
    child: Child,
}

impl AudioPreview {
    pub fn available() -> bool {
        tools::ffplay().is_some()
    }

    pub fn start(path: &Path, start: f64, duration: f64, volume: f32) -> Option<Self> {
        let ffplay = tools::ffplay()?;
        let child = tools::command(ffplay)
            .args([
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "quiet",
                "-vn",
                "-ss",
                &format!("{start:.3}"),
                "-t",
                &format!("{:.3}", duration.max(0.05)),
                "-af",
                &format!("volume={volume:.2}"),
            ])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        Some(Self { child })
    }
}

impl Drop for AudioPreview {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
