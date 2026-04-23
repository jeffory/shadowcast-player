use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig, SupportedStreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

/// Convert an f32 sample in [-1.0, 1.0] to i16, clamping out-of-range values.
#[inline]
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Convert an i16 sample to f32 in [-1.0, 1.0].
#[inline]
fn i16_to_f32(s: i16) -> f32 {
    s as f32 / -(i16::MIN as f32)
}

/// Sample formats we know how to convert to/from `i16`. Ordered by preference
/// (I16 first so we avoid conversion, F32 next, I8 last as a narrow fallback
/// for ALSA configurations that expose nothing else).
fn format_preference(fmt: SampleFormat) -> Option<u8> {
    match fmt {
        SampleFormat::I16 => Some(0),
        SampleFormat::F32 => Some(1),
        SampleFormat::I8 => Some(2),
        _ => None,
    }
}

/// Pick a supported input config for the given device, preferring 48kHz stereo
/// in a sample format we actually handle. Some ALSA setups expose `I8` as the
/// device default, which the stream builder below can't consume — filtering
/// here means we pick a usable format up-front instead of bailing at start.
fn pick_input_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    let target_rate = SampleRate(48000);
    if let Ok(ranges) = device.supported_input_configs() {
        let mut best: Option<(u8, cpal::SupportedStreamConfigRange)> = None;
        for range in ranges {
            if range.channels() != 2
                || range.min_sample_rate() > target_rate
                || range.max_sample_rate() < target_rate
            {
                continue;
            }
            let Some(pref) = format_preference(range.sample_format()) else {
                continue;
            };
            if best.as_ref().map_or(true, |(p, _)| pref < *p) {
                best = Some((pref, range));
            }
        }
        if let Some((_, range)) = best {
            return Ok(range.with_sample_rate(target_rate));
        }
    }
    device
        .default_input_config()
        .context("Failed to query default input config")
}

/// Pick a supported output config for the given device matching (rate, channels)
/// if possible, otherwise the device's default. Prefers sample formats the
/// output callback below knows how to fill.
fn pick_output_config(
    device: &cpal::Device,
    rate: SampleRate,
    channels: u16,
) -> Result<SupportedStreamConfig> {
    if let Ok(ranges) = device.supported_output_configs() {
        let mut best: Option<(u8, cpal::SupportedStreamConfigRange)> = None;
        for range in ranges {
            if range.channels() != channels
                || range.min_sample_rate() > rate
                || range.max_sample_rate() < rate
            {
                continue;
            }
            let Some(pref) = format_preference(range.sample_format()) else {
                continue;
            };
            if best.as_ref().map_or(true, |(p, _)| pref < *p) {
                best = Some((pref, range));
            }
        }
        if let Some((_, range)) = best {
            return Ok(range.with_sample_rate(rate));
        }
    }
    device
        .default_output_config()
        .context("Failed to query default output config")
}

/// Scales audio samples by the given volume factor, clamping to i16 range.
pub fn scale_volume(samples: &[i16], volume: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let scaled = (s as f32 * volume).round();
            scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect()
}

/// Trait for audio capture sources.
pub trait AudioSource {
    /// Starts the audio capture and playback streams.
    fn start(&mut self) -> Result<()>;

    /// Sets the volume level (0.0 = mute, 1.0 = full).
    fn set_volume(&self, volume: f32);

    /// Stops the audio capture and playback streams.
    fn stop(&mut self);
}

/// Trait for receiving captured audio samples.
pub trait AudioSampleReceiver {
    /// Returns the channel receiver for encoded audio samples, if available.
    fn audio_receiver(&self) -> Option<Receiver<Vec<i16>>>;
}

/// Audio capture source using cpal for input/output streaming.
///
/// Captures audio from a named input device (e.g. the ShadowCast 2),
/// plays it through the default output device for live monitoring, and optionally
/// sends samples to an encoder channel when recording is enabled.
pub struct CpalAudioSource {
    device_names: Vec<String>,
    volume: Arc<AtomicU32>,
    recording: Arc<AtomicBool>,
    input_stream: Option<Stream>,
    output_stream: Option<Stream>,
    sender: Sender<Vec<i16>>,
    receiver: Receiver<Vec<i16>>,
}

impl CpalAudioSource {
    /// Creates a new CpalAudioSource targeting the given input device name(s).
    ///
    /// Each name is matched as a case-insensitive substring against available audio
    /// device names. The first match wins. Provide multiple candidates to handle
    /// platform differences (e.g. ALSA card names vs product names).
    pub fn new(device_names: &[&str]) -> Self {
        let (sender, receiver) = bounded(64);
        Self {
            device_names: device_names.iter().map(|s| s.to_string()).collect(),
            volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            recording: Arc::new(AtomicBool::new(false)),
            input_stream: None,
            output_stream: None,
            sender,
            receiver,
        }
    }

    /// Enables or disables sending captured samples to the encoder channel.
    pub fn set_recording(&self, recording: bool) {
        self.recording.store(recording, Ordering::Relaxed);
    }
}

impl AudioSource for CpalAudioSource {
    fn start(&mut self) -> Result<()> {
        let host = cpal::default_host();

        // Find the input device matching one of the requested names.
        // Device names vary by platform (ALSA card names on Linux, CoreAudio/WASAPI
        // device names on macOS/Windows), so we try each candidate as a
        // case-insensitive substring match, first match wins.
        let input_devices: Vec<_> = host
            .input_devices()
            .context("Failed to enumerate input devices")?
            .collect();

        for d in &input_devices {
            log::debug!("Audio input device: '{}'", d.name().unwrap_or_default());
        }

        let match_names: Vec<String> = self.device_names.iter().map(|s| s.to_lowercase()).collect();

        // Collect all matching devices, then pick the best one.
        // Prefer direct ALSA devices (hw:, front:) over plugin devices
        // (sysdefault:, default) for lower latency.
        let mut matched: Vec<_> = match_names
            .iter()
            .flat_map(|pattern| {
                input_devices.iter().filter(move |d| {
                    let name = d.name().unwrap_or_default().to_lowercase();
                    name.contains(pattern)
                })
            })
            .collect();

        // Sort: prefer front: > hw: > plughw: > sysdefault: > others
        matched.sort_by_key(|d| {
            let name = d.name().unwrap_or_default().to_lowercase();
            if name.starts_with("front:") {
                0
            } else if name.starts_with("hw:") {
                1
            } else if name.starts_with("plughw:") {
                2
            } else if name.starts_with("sysdefault:") {
                3
            } else {
                4
            }
        });
        matched.dedup_by(|a, b| a.name().unwrap_or_default() == b.name().unwrap_or_default());

        let input_device = matched.first().context(format!(
            "No input device found matching any of: {:?}",
            self.device_names
        ))?;

        // Probe the device for a config it actually supports. On macOS,
        // CoreAudio typically exposes capture devices as f32 only, so
        // hardcoding i16 fails with "stream configuration not supported".
        let input_supported =
            pick_input_config(input_device).context("No compatible input config")?;
        let input_sample_format = input_supported.sample_format();
        let input_config: StreamConfig = input_supported.into();

        log::info!(
            "Using audio input device: {} @ {}Hz, {}ch, {:?}",
            input_device.name().unwrap_or_default(),
            input_config.sample_rate.0,
            input_config.channels,
            input_sample_format
        );

        if input_config.sample_rate.0 != 48000 {
            log::warn!(
                "Input running at {}Hz; encoder expects 48000Hz. Recording audio \
                 may be pitch-shifted until resampling is added.",
                input_config.sample_rate.0
            );
        }

        // Ring buffer for passing audio from input to output (~100ms at 48kHz stereo)
        let ring = HeapRb::<i16>::new(9600);
        let (mut producer, mut consumer) = ring.split();

        let err_fn = |err| log::error!("Audio input stream error: {}", err);
        let input_stream = match input_sample_format {
            SampleFormat::I16 => {
                let volume = Arc::clone(&self.volume);
                let recording = Arc::clone(&self.recording);
                let sender = self.sender.clone();
                input_device
                    .build_input_stream(
                        &input_config,
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                            let scaled = scale_volume(data, vol);
                            for &s in &scaled {
                                let _ = producer.try_push(s);
                            }
                            if recording.load(Ordering::Relaxed) {
                                let _ = sender.try_send(scaled);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .context("Failed to build audio input stream")?
            }
            SampleFormat::F32 => {
                let volume = Arc::clone(&self.volume);
                let recording = Arc::clone(&self.recording);
                let sender = self.sender.clone();
                input_device
                    .build_input_stream(
                        &input_config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                            let mut scaled: Vec<i16> = Vec::with_capacity(data.len());
                            for &f in data {
                                let s = f32_to_i16(f);
                                let v = ((s as f32 * vol).round())
                                    .clamp(i16::MIN as f32, i16::MAX as f32)
                                    as i16;
                                scaled.push(v);
                            }
                            for &s in &scaled {
                                let _ = producer.try_push(s);
                            }
                            if recording.load(Ordering::Relaxed) {
                                let _ = sender.try_send(scaled);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .context("Failed to build audio input stream")?
            }
            SampleFormat::I8 => {
                let volume = Arc::clone(&self.volume);
                let recording = Arc::clone(&self.recording);
                let sender = self.sender.clone();
                input_device
                    .build_input_stream(
                        &input_config,
                        move |data: &[i8], _: &cpal::InputCallbackInfo| {
                            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                            let mut scaled: Vec<i16> = Vec::with_capacity(data.len());
                            for &s in data {
                                let widened = (s as i16) << 8;
                                let v = ((widened as f32 * vol).round())
                                    .clamp(i16::MIN as f32, i16::MAX as f32)
                                    as i16;
                                scaled.push(v);
                            }
                            for &s in &scaled {
                                let _ = producer.try_push(s);
                            }
                            if recording.load(Ordering::Relaxed) {
                                let _ = sender.try_send(scaled);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .context("Failed to build audio input stream")?
            }
            other => {
                anyhow::bail!("Unsupported audio input sample format: {:?}", other);
            }
        };

        input_stream
            .play()
            .context("Failed to start audio input stream")?;

        // Output stream: read from ring buffer for live monitoring. The output
        // device may use a different native sample format than the input, and
        // default outputs on macOS are usually f32.
        let output_device = host
            .default_output_device()
            .context("No default output device found")?;

        let output_supported = pick_output_config(
            &output_device,
            input_config.sample_rate,
            input_config.channels,
        )
        .context("No compatible output config")?;
        let output_sample_format = output_supported.sample_format();
        let output_config: StreamConfig = output_supported.into();

        log::info!(
            "Using audio output device: {} @ {}Hz, {}ch, {:?}",
            output_device.name().unwrap_or_default(),
            output_config.sample_rate.0,
            output_config.channels,
            output_sample_format
        );

        let out_err_fn = |err| log::error!("Audio output stream error: {}", err);
        let output_stream = match output_sample_format {
            SampleFormat::I16 => output_device
                .build_output_stream(
                    &output_config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        for sample in data.iter_mut() {
                            *sample = consumer.try_pop().unwrap_or(0);
                        }
                    },
                    out_err_fn,
                    None,
                )
                .context("Failed to build audio output stream")?,
            SampleFormat::F32 => output_device
                .build_output_stream(
                    &output_config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        for sample in data.iter_mut() {
                            *sample = i16_to_f32(consumer.try_pop().unwrap_or(0));
                        }
                    },
                    out_err_fn,
                    None,
                )
                .context("Failed to build audio output stream")?,
            SampleFormat::I8 => output_device
                .build_output_stream(
                    &output_config,
                    move |data: &mut [i8], _: &cpal::OutputCallbackInfo| {
                        for sample in data.iter_mut() {
                            *sample = (consumer.try_pop().unwrap_or(0) >> 8) as i8;
                        }
                    },
                    out_err_fn,
                    None,
                )
                .context("Failed to build audio output stream")?,
            other => {
                anyhow::bail!("Unsupported audio output sample format: {:?}", other);
            }
        };

        output_stream
            .play()
            .context("Failed to start audio output stream")?;

        self.input_stream = Some(input_stream);
        self.output_stream = Some(output_stream);

        Ok(())
    }

    fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn stop(&mut self) {
        // Dropping the streams stops capture/playback
        self.input_stream = None;
        self.output_stream = None;
    }
}

impl AudioSampleReceiver for CpalAudioSource {
    fn audio_receiver(&self) -> Option<Receiver<Vec<i16>>> {
        Some(self.receiver.clone())
    }
}
