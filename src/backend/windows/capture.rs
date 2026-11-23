use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, ImageFormat, VideoEncoder,
    VideoSettingsBuilder, VideoSettingsSubType,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;

use super::webcam::{self, WebcamStream};
use super::{BoxError, SharedEncoder, SharedError};
use crate::media::{Codec, Region};

#[derive(Clone)]
pub struct CaptureFlags {
    pub encoder_slot: SharedEncoder,
    pub error_slot: SharedError,
    pub frames: Arc<AtomicU64>,
    pub stop: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub audio: bool,
    pub codec: Codec,
    pub output_path: PathBuf,
    pub region: Option<Region>,
    pub webcam: Option<WebcamStream>,
}

pub struct CaptureHandler {
    encoder: SharedEncoder,
    error: SharedError,
    frames: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    region: Option<Region>,
    webcam: Option<WebcamStream>,
    paused_ticks: i64,
    pause_mark: Option<Instant>,
    buffer_mode: bool,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = CaptureFlags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let flags = ctx.flags;

        // Video encoders require dimensions to be even numbers
        let width = ((flags.width / 2) * 2).max(2);
        let height = ((flags.height / 2) * 2).max(2);

        let video = VideoSettingsBuilder::new(width, height)
            .bitrate(flags.bitrate)
            .frame_rate(flags.fps)
            .sub_type(match flags.codec {
                Codec::H264 => VideoSettingsSubType::H264,
                Codec::Hevc => VideoSettingsSubType::HEVC,
            });

        let audio = AudioSettingsBuilder::new().disabled(!flags.audio);

        let encoder = VideoEncoder::new(
            video,
            audio,
            ContainerSettingsBuilder::new(),
            &flags.output_path,
        )?;

        *flags.encoder_slot.lock() = Some(encoder);

        let buffer_mode = flags.region.is_some() || flags.webcam.is_some();

        Ok(Self {
            encoder: flags.encoder_slot,
            error: flags.error_slot,
            frames: flags.frames,
            stop: flags.stop,
            paused: flags.paused,
            region: flags.region,
            webcam: flags.webcam,
            paused_ticks: 0,
            pause_mark: None,
            buffer_mode,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }

        // When paused we simply drop frames. Pause duration is subtracted from
        // the video timestamps after resuming so video and audio stay in sync.
        if self.paused.load(Ordering::Relaxed) {
            if self.pause_mark.is_none() {
                self.pause_mark = Some(Instant::now());
            }
            return Ok(());
        }

        if let Some(mark) = self.pause_mark.take() {
            self.paused_ticks = self
                .paused_ticks
                .saturating_add(mark.elapsed().as_nanos() as i64 / 100);
            // From now on we need full control over timestamps.
            self.buffer_mode = true;
        }

        let absolute = frame
            .timestamp()
            .map_err(|err| anyhow::anyhow!("timestamp error: {err}"))?
            .Duration;
        // The encoder subtracts the timestamp of the very first frame it sees
        // (whether it arrives through the DirectX or the raw-buffer path), so
        // pass an absolute timestamp here. The mixed-path case after a pause is
        // handled correctly because both paths share that first timestamp.
        let timestamp = (absolute - self.paused_ticks).max(0);

        self.frames.fetch_add(1, Ordering::Relaxed);

        let prepared = if self.buffer_mode || self.region.is_some() || self.webcam.is_some() {
            match self.build_buffer(frame) {
                Ok(buffer) => Some(buffer),
                Err(err) => {
                    *self.error.lock() = Some(format!("Frame conversion error: {err}"));
                    return Ok(());
                }
            }
        } else {
            None
        };

        let mut slot = self.encoder.lock();
        let Some(encoder) = slot.as_mut() else {
            return Ok(());
        };

        let result = match &prepared {
            Some(buffer) => encoder.send_frame_buffer(buffer, timestamp),
            None => encoder.send_frame(frame),
        };

        if let Err(err) = result {
            *self.error.lock() = Some(format!("Encoder error: {err}"));
            capture_control.stop();
        }

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        *self.error.lock() = Some("The captured window/monitor was closed.".to_string());
        Ok(())
    }
}

impl CaptureHandler {
    /// Converts a captured frame into a tightly packed, bottom-up BGRA buffer
    /// (the layout expected by the raw-buffer encoder path), applying a region
    /// crop and the optional webcam overlay on the way.
    fn build_buffer(&mut self, frame: &mut Frame) -> Result<Vec<u8>, BoxError> {
        let frame_buffer = if let Some(region) = self.region {
            let x0 = region.x.min(frame.width().saturating_sub(1));
            let y0 = region.y.min(frame.height().saturating_sub(1));
            let x1 = (x0 + region.width).min(frame.width());
            let y1 = (y0 + region.height).min(frame.height());
            frame
                .buffer_crop(x0, y0, x1, y1)
                .map_err(|err| anyhow::anyhow!("{err}"))?
        } else {
            frame.buffer().map_err(|err| anyhow::anyhow!("{err}"))?
        };

        let width = frame_buffer.width();
        let height = frame_buffer.height();
        let stride = width as usize * 4;

        let mut raw = Vec::new();
        let source = frame_buffer.as_nopadding_buffer(&mut raw);
        let needed = stride * height as usize;
        let mut data = source.get(..needed).unwrap_or(source).to_vec();

        if let Some(camera) = &self.webcam {
            if let Some(camera_frame) = camera.latest_frame() {
                webcam::composite(&mut data, width, height, &camera_frame, &camera.overlay());
            }
        }

        flip_vertical(&mut data, stride, height);

        Ok(data)
    }
}

fn flip_vertical(data: &mut [u8], stride: usize, height: u32) {
    if height < 2 {
        return;
    }
    let mut scratch = vec![0u8; stride];
    for y in 0..(height / 2) as usize {
        let top = y * stride;
        let bottom = (height as usize - 1 - y) * stride;
        if bottom + stride > data.len() || top + stride > data.len() {
            break;
        }
        scratch.copy_from_slice(&data[top..top + stride]);
        data.copy_within(bottom..bottom + stride, top);
        data[bottom..bottom + stride].copy_from_slice(&scratch);
    }
}

// ---------------------------------------------------------------------------
// One-shot screenshot capture
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ScreenshotFlags {
    pub output: PathBuf,
    pub region: Option<Region>,
    pub result: Arc<Mutex<Option<Result<PathBuf, String>>>>,
}

pub struct ScreenshotHandler {
    flags: ScreenshotFlags,
}

impl GraphicsCaptureApiHandler for ScreenshotHandler {
    type Flags = ScreenshotFlags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let outcome = if let Some(region) = self.flags.region {
            let x0 = region.x.min(frame.width().saturating_sub(1));
            let y0 = region.y.min(frame.height().saturating_sub(1));
            let x1 = (x0 + region.width).min(frame.width());
            let y1 = (y0 + region.height).min(frame.height());
            frame
                .buffer_crop(x0, y0, x1, y1)
                .map_err(|err| err.to_string())
                .and_then(|mut buffer| {
                    buffer
                        .save_as_image(&self.flags.output, ImageFormat::Png)
                        .map_err(|err| err.to_string())
                })
        } else {
            frame
                .save_as_image(&self.flags.output, ImageFormat::Png)
                .map_err(|err| err.to_string())
        };

        let result = match outcome {
            Ok(()) => Ok(self.flags.output.clone()),
            Err(err) => Err(err),
        };
        *self.flags.result.lock() = Some(result);
        capture_control.stop();

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        let mut slot = self.flags.result.lock();
        if slot.is_none() {
            *slot = Some(Err("The captured window/monitor was closed.".to_string()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// One-shot raw grab (used for the region-picker preview)
// ---------------------------------------------------------------------------

pub struct GrabFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed BGRA pixels, top row first.
    pub bgra: Vec<u8>,
}

#[derive(Clone)]
pub struct GrabFlags {
    pub region: Option<Region>,
    pub result: Arc<Mutex<Option<Result<GrabFrame, String>>>>,
}

pub struct GrabHandler {
    flags: GrabFlags,
}

impl GraphicsCaptureApiHandler for GrabHandler {
    type Flags = GrabFlags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let outcome = (|| -> Result<GrabFrame, String> {
            let buffer = if let Some(region) = self.flags.region {
                let x0 = region.x.min(frame.width().saturating_sub(1));
                let y0 = region.y.min(frame.height().saturating_sub(1));
                let x1 = (x0 + region.width).min(frame.width());
                let y1 = (y0 + region.height).min(frame.height());
                frame
                    .buffer_crop(x0, y0, x1, y1)
                    .map_err(|err| err.to_string())?
            } else {
                frame.buffer().map_err(|err| err.to_string())?
            };

            let width = buffer.width();
            let height = buffer.height();
            let mut raw = Vec::new();
            let source = buffer.as_nopadding_buffer(&mut raw);
            let needed = width as usize * 4 * height as usize;
            let bgra = source.get(..needed).unwrap_or(source).to_vec();
            Ok(GrabFrame {
                width,
                height,
                bgra,
            })
        })();

        *self.flags.result.lock() = Some(outcome);
        capture_control.stop();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        let mut slot = self.flags.result.lock();
        if slot.is_none() {
            *slot = Some(Err("The captured source was closed.".to_string()));
        }
        Ok(())
    }
}
