use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, Stream, StreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

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

        log::info!(
            "Using audio input device: {}",
            input_device.name().unwrap_or_default()
        );

        // Configure 48kHz stereo i16. Let ALSA/PipeWire choose the buffer size —
        // not all devices (especially sysdefault: on PipeWire) support a fixed
        // period size via snd_pcm_hw_params_set_buffer_size.
        let config = StreamConfig {
            channels: 2,
            sample_rate: SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        // Ring buffer for passing audio from input to output (~100ms at 48kHz stereo)
        let ring = HeapRb::<i16>::new(9600);
        let (mut producer, mut consumer) = ring.split();

        let volume = Arc::clone(&self.volume);
        let recording = Arc::clone(&self.recording);
        let sender = self.sender.clone();

        // Input stream: capture samples, scale volume, push to ring buffer + channel
        let input_stream = input_device
            .build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                    let scaled = scale_volume(data, vol);

                    // Push to ring buffer for live playback (drop oldest if full)
                    for &sample in &scaled {
                        let _ = producer.try_push(sample);
                    }

                    // Send to encoder channel if recording
                    if recording.load(Ordering::Relaxed) {
                        let _ = sender.try_send(scaled);
                    }
                },
                |err| {
                    log::error!("Audio input stream error: {}", err);
                },
                None,
            )
            .context("Failed to build audio input stream")?;

        input_stream
            .play()
            .context("Failed to start audio input stream")?;

        // Output stream: read from ring buffer for live monitoring
        let output_device = host
            .default_output_device()
            .context("No default output device found")?;

        log::info!(
            "Using audio output device: {}",
            output_device.name().unwrap_or_default()
        );

        let output_stream = output_device
            .build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() {
                        *sample = consumer.try_pop().unwrap_or(0);
                    }
                },
                |err| {
                    log::error!("Audio output stream error: {}", err);
                },
                None,
            )
            .context("Failed to build audio output stream")?;

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
