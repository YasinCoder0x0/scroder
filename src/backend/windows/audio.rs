use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasapi::{
    initialize_mta, AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, SampleType,
    StreamMode, WaveFormat,
};

use super::{SharedEncoder, SharedError};

/// The audio format we ask WASAPI to convert to. It matches the encoder settings.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
const BYTES_PER_SAMPLE: usize = 2;
const BYTES_PER_FRAME: usize = BYTES_PER_SAMPLE * CHANNELS;

pub struct AudioHandles {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl AudioHandles {
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for AudioHandles {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Source {
    client: AudioClient,
    capture: AudioCaptureClient,
    queue: VecDeque<i16>,
    label: &'static str,
}

pub fn start(
    encoder: SharedEncoder,
    error: SharedError,
    system_audio: bool,
    microphone: bool,
    paused: Arc<AtomicBool>,
    audio_level: Arc<AtomicU32>,
) -> AudioHandles {
    let stop = Arc::new(AtomicBool::new(false));
    let join = thread::Builder::new()
        .name("scroder-audio".to_string())
        .spawn({
            let stop = stop.clone();
            move || {
                run(
                    encoder,
                    error,
                    system_audio,
                    microphone,
                    stop,
                    paused,
                    audio_level,
                )
            }
        })
        .ok();

    AudioHandles { stop, join }
}

fn run(
    encoder: SharedEncoder,
    error: SharedError,
    system_audio: bool,
    microphone: bool,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    audio_level: Arc<AtomicU32>,
) {
    let _ = initialize_mta();

    let mut sources: Vec<Source> = Vec::new();

    if system_audio {
        match open_source(true) {
            Ok(source) => sources.push(source),
            Err(err) => warn(&error, format!("System audio unavailable: {err}")),
        }
    }

    if microphone {
        match open_source(false) {
            Ok(source) => sources.push(source),
            Err(err) => warn(&error, format!("Microphone unavailable: {err}")),
        }
    }

    let start_time = Instant::now();
    let mut total_samples_sent: u64 = 0;
    let mut paused_duration = Duration::ZERO;
    let mut pause_start: Option<Instant> = None;
    let mut last_error_log = 0u8;

    const MAX_FRAMES_PER_TICK: usize = 4_800; // 100 ms

    while !stop.load(Ordering::Relaxed) {
        // Handle paused state
        if paused.load(Ordering::Relaxed) {
            if pause_start.is_none() {
                pause_start = Some(Instant::now());
            }
            // Discard pending source audio so buffers do not build up while paused
            for source in &mut sources {
                let _ = read_available(source);
                source.queue.clear();
            }
            audio_level.store(0, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(15));
            continue;
        }

        if let Some(p_start) = pause_start.take() {
            paused_duration += p_start.elapsed();
        }

        // Read available audio from each active source
        for source in &mut sources {
            if let Err(err) = read_available(source) {
                if last_error_log < 8 {
                    last_error_log += 1;
                    warn(&error, format!("{} audio read error: {err}", source.label));
                }
            }
        }

        // Calculate expected frame position based on real elapsed time (minus pause time)
        let effective_elapsed = start_time.elapsed().saturating_sub(paused_duration);
        let target_frames = (effective_elapsed.as_secs_f64() * SAMPLE_RATE as f64) as u64;

        if total_samples_sent < target_frames {
            let frames_needed =
                ((target_frames - total_samples_sent) as usize).min(MAX_FRAMES_PER_TICK);

            if frames_needed > 0 {
                let available = sources
                    .iter()
                    .map(|s| s.queue.len() / CHANNELS)
                    .max()
                    .unwrap_or(0);

                let frames_to_mix = frames_needed.min(available);
                let mut mixed = vec![0i16; frames_needed * CHANNELS];

                if frames_to_mix > 0 {
                    for source in &mut sources {
                        let take = (source.queue.len() / CHANNELS).min(frames_to_mix);
                        for k in 0..take * CHANNELS {
                            mixed[k] = mixed[k].saturating_add(source.queue[k]);
                        }
                        source.queue.drain(0..take * CHANNELS);
                    }
                }

                // Compute peak level for visual audio meter (0..100)
                let mut peak: i16 = 0;
                for &sample in &mixed {
                    peak = peak.max(sample.abs());
                }
                let pct = ((peak as u32 * 100) / (i16::MAX as u32 / 2)).min(100);
                audio_level.store(pct, Ordering::Relaxed);

                // Convert 16-bit PCM to byte buffer
                let mut bytes = Vec::with_capacity(mixed.len() * BYTES_PER_SAMPLE);
                for sample in &mixed {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }

                let mut slot = encoder.lock();
                if let Some(enc) = slot.as_mut() {
                    if let Err(err) = enc.send_audio_buffer(&bytes, 0) {
                        drop(slot);
                        warn(&error, format!("Audio encoder error: {err}"));
                    }
                }

                total_samples_sent += frames_needed as u64;
            }
        } else {
            // Keep UI audio meter smooth if no samples sent in this slice
            audio_level.store(0, Ordering::Relaxed);
        }

        // Keep queues bounded if a device stalls
        for source in &mut sources {
            if source.queue.len() > SAMPLE_RATE as usize * CHANNELS * 2 {
                source.queue.clear();
            }
        }

        thread::sleep(Duration::from_millis(6));
    }

    for source in sources {
        let _ = source.client.stop_stream();
    }
}

fn open_source(loopback: bool) -> anyhow::Result<Source> {
    let enumerator = DeviceEnumerator::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let device_dir = if loopback {
        Direction::Render
    } else {
        Direction::Capture
    };
    let device = enumerator
        .get_default_device(device_dir)
        .map_err(|e| anyhow::anyhow!("no default device: {e}"))?;

    let mut client = device
        .get_iaudioclient()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let format = WaveFormat::new(
        16,
        16,
        SampleType::Int,
        SAMPLE_RATE as usize,
        CHANNELS,
        None,
    );

    let (default_period, _min_period) = client
        .get_device_period()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let buffer_duration = default_period.max(100_000).max(500_000);

    let mode = StreamMode::PollingShared {
        autoconvert: true,
        buffer_duration_hns: buffer_duration,
    };

    // For loopback we open the render endpoint and initialize a capture stream on it.
    client
        .initialize_client(&format, Direction::Capture, mode, None)
        .map_err(|e| anyhow::anyhow!("initialize failed: {e}"))?;

    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    client
        .start_stream()
        .map_err(|e| anyhow::anyhow!("start failed: {e}"))?;

    Ok(Source {
        client,
        capture,
        queue: VecDeque::with_capacity(SAMPLE_RATE as usize * CHANNELS),
        label: if loopback { "System" } else { "Microphone" },
    })
}

fn read_available(source: &mut Source) -> Result<(), wasapi::WasapiError> {
    loop {
        match source.capture.get_next_packet_size()? {
            Some(frames) if frames > 0 => {
                let mut buffer = vec![0u8; frames as usize * BYTES_PER_FRAME];
                let (frames_read, _info) = source.capture.read_from_device(&mut buffer)?;
                let usable = frames_read as usize * BYTES_PER_FRAME;
                for chunk in buffer[..usable].chunks_exact(BYTES_PER_SAMPLE) {
                    source
                        .queue
                        .push_back(i16::from_le_bytes([chunk[0], chunk[1]]));
                }
            }
            _ => break,
        }
    }
    Ok(())
}

fn warn(error: &SharedError, message: String) {
    let mut slot = error.lock();
    if slot.is_none() {
        *slot = Some(message);
    }
}
