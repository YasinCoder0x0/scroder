use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromMediaSource,
    MFEnumDeviceSources, MFMediaType_Video, MFStartup, MFVideoFormat_RGB32,
    MFVideoInterlace_Progressive, MFSTARTUP_FULL, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::media::{Corner, WebcamOptions};

#[derive(Clone, Copy, Debug)]
pub struct WebcamOverlay {
    pub size_pct: u8,
    pub corner: Corner,
    pub mirror: bool,
}

#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub struct WebcamStream {
    inner: Arc<Inner>,
}

struct Inner {
    latest: Mutex<Option<Arc<CameraFrame>>>,
    stop: AtomicBool,
    overlay: WebcamOverlay,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.lock().take() {
            let _ = join.join();
        }
    }
}

impl WebcamStream {
    pub fn latest_frame(&self) -> Option<Arc<CameraFrame>> {
        self.inner.latest.lock().clone()
    }

    pub fn overlay(&self) -> WebcamOverlay {
        self.inner.overlay
    }
}

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn mf_init() -> Result<()> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        com_init();
        unsafe {
            let _ = MFStartup(131184, MFSTARTUP_FULL);
        }
    });
    Ok(())
}

/// Enumerates available video capture devices. Returns human-readable names.
pub fn enumerate_devices() -> Vec<String> {
    if mf_init().is_err() {
        return Vec::new();
    }

    unsafe {
        let mut attributes: Option<IMFAttributes> = None;
        if MFCreateAttributes(&mut attributes, 1).is_err() {
            return Vec::new();
        }
        let Some(attributes) = attributes else {
            return Vec::new();
        };
        if attributes
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .is_err()
        {
            return Vec::new();
        }

        let mut raw: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count: u32 = 0;
        if MFEnumDeviceSources(&attributes, &mut raw, &mut count).is_err() {
            return Vec::new();
        }

        let mut names = Vec::new();
        if !raw.is_null() {
            let devices = std::slice::from_raw_parts(raw, count as usize);
            for device in devices {
                if let Some(device) = device {
                    let mut buffer = [0u16; 512];
                    if device
                        .GetString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut buffer, None)
                        .is_ok()
                    {
                        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
                        let name = String::from_utf16_lossy(&buffer[..end]);
                        names.push(if name.trim().is_empty() {
                            format!("Camera {}", names.len() + 1)
                        } else {
                            name
                        });
                    } else {
                        names.push(format!("Camera {}", names.len() + 1));
                    }
                }
            }
        }

        names
    }
}

pub fn start(options: WebcamOptions) -> Result<WebcamStream> {
    mf_init()?;

    let inner = Arc::new(Inner {
        latest: Mutex::new(None),
        stop: AtomicBool::new(false),
        overlay: WebcamOverlay {
            size_pct: options.size_pct.clamp(8, 60),
            corner: options.corner,
            mirror: options.mirror,
        },
        join: Mutex::new(None),
    });

    let (tx, rx) = mpsc::channel();
    let thread_inner = inner.clone();
    let handle = thread::Builder::new()
        .name("screenrec-webcam".to_string())
        .spawn(move || run(thread_inner, options, tx))
        .map_err(|err| anyhow!("failed to spawn webcam thread: {err}"))?;

    *inner.join.lock() = Some(handle);

    match rx.recv_timeout(Duration::from_secs(6)) {
        Ok(Ok(())) => Ok(WebcamStream { inner }),
        Ok(Err(err)) => {
            inner.stop.store(true, Ordering::SeqCst);
            bail!("{err}");
        }
        Err(_) => {
            inner.stop.store(true, Ordering::SeqCst);
            bail!("the camera did not produce a frame in time");
        }
    }
}

fn packed(size_a: u32, size_b: u32) -> u64 {
    ((size_a as u64) << 32) | size_b as u64
}

fn unpack_size(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

fn run(inner: Arc<Inner>, options: WebcamOptions, ready: Sender<Result<(), String>>) {
    let mut ready = Some(ready);
    let mut signal = |result: Result<(), String>| {
        if let Some(tx) = ready.take() {
            let _ = tx.send(result);
        }
    };

    com_init();

    match run_inner(&inner, &options, &mut signal) {
        Ok(()) => {}
        Err(err) => signal(Err(format!("{err:#}"))),
    }

    unsafe {
        CoUninitialize();
    }
}

fn run_inner(
    inner: &Arc<Inner>,
    options: &WebcamOptions,
    signal: &mut dyn FnMut(Result<(), String>),
) -> Result<()> {
    unsafe {
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.ok_or_else(|| anyhow!("could not create attributes"))?;
        attributes.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;

        let mut raw: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count: u32 = 0;
        MFEnumDeviceSources(&attributes, &mut raw, &mut count)?;
        if raw.is_null() || count == 0 {
            bail!("no camera found");
        }

        let devices = std::slice::from_raw_parts(raw, count as usize);
        let device = devices
            .get(options.device_index.min(count as usize - 1))
            .and_then(|d| d.as_ref())
            .ok_or_else(|| anyhow!("camera index out of range"))?;

        let source: IMFMediaSource = device.ActivateObject()?;

        let mut reader_attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut reader_attributes, 2)?;
        if let Some(reader_attributes) = &reader_attributes {
            let _ =
                reader_attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1);
            let _ = reader_attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
        }

        let reader: IMFSourceReader =
            MFCreateSourceReaderFromMediaSource(&source, reader_attributes.as_ref())?;

        let stream_index = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

        let media_type: IMFMediaType = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, packed(1280, 720))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, packed(30, 1))?;
        reader.SetCurrentMediaType(stream_index, None, &media_type)?;

        let current = reader.GetCurrentMediaType(stream_index)?;
        let (width, height) = current
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map(unpack_size)
            .unwrap_or((1280, 720));
        if width == 0 || height == 0 {
            bail!("the camera reported an invalid frame size");
        }

        let stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map(|value| value as i32)
            .unwrap_or(-1);
        let bottom_up = stride <= 0;
        let src_stride = if stride == 0 {
            width as usize * 4
        } else {
            stride.unsigned_abs() as usize
        };
        let row_len = width as usize * 4;
        let frame_len = row_len * height as usize;

        loop {
            if inner.stop.load(Ordering::Relaxed) {
                break;
            }

            let mut actual_stream = 0u32;
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(
                stream_index,
                0,
                Some(&mut actual_stream),
                Some(&mut flags),
                None,
                Some(&mut sample),
            )?;

            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                break;
            }

            let Some(sample) = sample else {
                thread::sleep(Duration::from_millis(2));
                continue;
            };

            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data_ptr: *mut u8 = ptr::null_mut();
            let mut max_len = 0u32;
            let mut current_len = 0u32;
            buffer.Lock(&mut data_ptr, Some(&mut max_len), Some(&mut current_len))?;

            if !data_ptr.is_null() && current_len > 0 {
                let src = std::slice::from_raw_parts(data_ptr, current_len as usize);
                let mut frame = vec![0u8; frame_len];
                let mut ok = true;
                for y in 0..height as usize {
                    let src_row = if bottom_up {
                        height as usize - 1 - y
                    } else {
                        y
                    };
                    let start = src_row * src_stride;
                    if start + row_len > src.len() {
                        ok = false;
                        break;
                    }
                    frame[y * row_len..(y + 1) * row_len]
                        .copy_from_slice(&src[start..start + row_len]);
                }
                if ok {
                    *inner.latest.lock() = Some(Arc::new(CameraFrame {
                        width,
                        height,
                        data: frame,
                    }));
                    signal(Ok(()));
                }
            }

            let _ = buffer.Unlock();
        }
    }

    Ok(())
}

/// Draws the camera frame on top of `base` (top-down BGRA) in the selected corner.
pub fn composite(
    base: &mut [u8],
    base_width: u32,
    base_height: u32,
    camera: &CameraFrame,
    overlay: &WebcamOverlay,
) {
    if base_width < 8 || base_height < 8 || camera.width == 0 || camera.height == 0 {
        return;
    }

    let margin = ((base_height as f32 * 0.02).round() as u32).max(8);
    let mut dest_h = ((base_height as f32 * overlay.size_pct as f32 / 100.0).round() as u32)
        .clamp(16, base_height.saturating_sub(margin * 2).max(16));
    let aspect = camera.width as f32 / camera.height as f32;
    let mut dest_w = (dest_h as f32 * aspect).round() as u32;
    let max_w = base_width.saturating_sub(margin * 2).max(16);
    if dest_w > max_w {
        dest_w = max_w;
        dest_h = (dest_w as f32 / aspect).round() as u32;
    }
    if dest_w < 8 || dest_h < 8 {
        return;
    }

    let (origin_x, origin_y) = match overlay.corner {
        Corner::TopLeft => (margin, margin),
        Corner::TopRight => (base_width.saturating_sub(dest_w + margin), margin),
        Corner::BottomLeft => (margin, base_height.saturating_sub(dest_h + margin)),
        Corner::BottomRight => (
            base_width.saturating_sub(dest_w + margin),
            base_height.saturating_sub(dest_h + margin),
        ),
    };

    let radius = (dest_w.min(dest_h) as f32 * 0.10).max(6.0);
    let base_stride = base_width as usize * 4;
    let cam_stride = camera.width as usize * 4;

    for dy in 0..dest_h {
        let py = origin_y + dy;
        if py >= base_height {
            break;
        }
        for dx in 0..dest_w {
            let px = origin_x + dx;
            if px >= base_width {
                break;
            }
            if !inside_rounded(dx, dy, dest_w, dest_h, radius) {
                continue;
            }

            let mut sx = ((dx as f32 * camera.width as f32) / dest_w as f32) as u32;
            let sy = ((dy as f32 * camera.height as f32) / dest_h as f32) as u32;
            sx = sx.min(camera.width - 1);
            let sy = sy.min(camera.height - 1);
            let sx = if overlay.mirror {
                camera.width - 1 - sx
            } else {
                sx
            };

            let src_index = sy as usize * cam_stride + sx as usize * 4;
            let dst_index = py as usize * base_stride + px as usize * 4;
            if src_index + 4 <= camera.data.len() && dst_index + 4 <= base.len() {
                base[dst_index..dst_index + 4]
                    .copy_from_slice(&camera.data[src_index..src_index + 4]);
            }
        }
    }

    draw_rounded_border(
        base,
        base_width,
        base_height,
        origin_x,
        origin_y,
        dest_w,
        dest_h,
        radius,
    );
}

fn inside_rounded(dx: u32, dy: u32, width: u32, height: u32, radius: f32) -> bool {
    let x = dx as f32 + 0.5;
    let y = dy as f32 + 0.5;
    let w = width as f32;
    let h = height as f32;
    let (cx, cy) = if x < radius && y < radius {
        (radius, radius)
    } else if x > w - radius && y < radius {
        (w - radius, radius)
    } else if x < radius && y > h - radius {
        (radius, h - radius)
    } else if x > w - radius && y > h - radius {
        (w - radius, h - radius)
    } else {
        return true;
    };

    let ddx = x - cx;
    let ddy = y - cy;
    ddx * ddx + ddy * ddy <= radius * radius
}

#[allow(clippy::too_many_arguments)]
fn draw_rounded_border(
    base: &mut [u8],
    base_width: u32,
    base_height: u32,
    origin_x: u32,
    origin_y: u32,
    width: u32,
    height: u32,
    radius: f32,
) {
    let color = [0xFF, 0x4B, 0x48, 0xFF]; // #484BFF primary (BGRA)
    let thickness = 2u32;
    let stride = base_width as usize * 4;

    for dy in 0..height {
        let py = origin_y + dy;
        if py >= base_height {
            break;
        }
        for dx in 0..width {
            let px = origin_x + dx;
            if px >= base_width {
                break;
            }
            let edge = dx < thickness
                || dy < thickness
                || dx + thickness >= width
                || dy + thickness >= height;
            if !edge || !inside_rounded(dx, dy, width, height, radius) {
                continue;
            }
            let index = py as usize * stride + px as usize * 4;
            if index + 4 <= base.len() {
                base[index..index + 4].copy_from_slice(&color);
            }
        }
    }
}
