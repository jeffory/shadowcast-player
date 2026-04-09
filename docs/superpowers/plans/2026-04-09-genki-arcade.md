# genki-arcade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a low-latency video capture viewer for the GENKI Shadowcast 2 with a hidable toolbar for resolution switching, volume control, recording, and screenshots.

**Architecture:** V4L2 captures video frames from `/dev/video2`, decodes MJPEG/YUYV to RGB, uploads to wgpu GPU textures for display. cpal handles audio capture/playback. egui draws a toolbar overlay. Recording uses ffmpeg-next on a dedicated thread. Traits (`VideoSource`, `AudioSource`, `Encoder`) enable TDD with mocks.

**Tech Stack:** Rust, v4l, wgpu, winit, egui, cpal, ffmpeg-next, zune-jpeg, crossbeam-channel, ringbuf

**Spec:** `docs/superpowers/specs/2026-04-09-genki-arcade-design.md`

---

## File Structure

```
genki-arcade/
├── Cargo.toml
├── src/
│   ├── main.rs                  # Entry point, event loop, wires subsystems together
│   ├── app.rs                   # ApplicationHandler impl, owns all state
│   ├── capture/
│   │   ├── mod.rs               # Re-exports
│   │   ├── video.rs             # VideoSource trait + V4L2 implementation
│   │   ├── audio.rs             # AudioSource trait + cpal implementation
│   │   └── format.rs            # CaptureFormat, PixelFormat, Frame types + conversion
│   ├── render/
│   │   ├── mod.rs               # Re-exports
│   │   ├── display.rs           # wgpu setup, texture upload, quad rendering pipeline
│   │   ├── shader.wgsl          # Vertex + fragment shader for textured quad
│   │   └── overlay.rs           # egui toolbar: volume, resolution, record, screenshot
│   └── record/
│       ├── mod.rs               # Re-exports
│       ├── encoder.rs           # Encoder trait + ffmpeg-next implementation
│       └── screenshot.rs        # PNG screenshot capture
├── tests/
│   ├── format_test.rs           # Frame conversion unit tests
│   ├── audio_test.rs            # Volume scaling tests
│   ├── encoder_test.rs          # Encoder trait mock tests
│   └── screenshot_test.rs       # Screenshot path/naming tests
└── docs/
    └── superpowers/
        ├── specs/
        │   └── 2026-04-09-genki-arcade-design.md
        └── plans/
            └── 2026-04-09-genki-arcade.md  (this file)
```

---

## Task 1: Project Scaffold + Data Types

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/capture/mod.rs`
- Create: `src/capture/format.rs`
- Create: `src/render/mod.rs`
- Create: `src/record/mod.rs`
- Test: `tests/format_test.rs`

- [ ] **Step 1: Initialize the Rust project**

Run:
```bash
cd /home/keith/Projects/Rust/genki-arcade
git init
cargo init --name genki-arcade
```
Expected: `Cargo.toml` and `src/main.rs` created.

- [ ] **Step 2: Write Cargo.toml with all dependencies**

Replace `Cargo.toml` with:
```toml
[package]
name = "genki-arcade"
version = "0.1.0"
edition = "2021"

[dependencies]
v4l = "0.14"
cpal = "0.15"
wgpu = "24"
winit = "0.30"
egui = "0.31"
egui-wgpu = "0.31"
egui-winit = "0.31"
zune-jpeg = "0.5"
ffmpeg-next = "7"
image = "0.25"
directories = "6"
chrono = "0.4"
crossbeam-channel = "0.5"
ringbuf = "0.4"
anyhow = "1"
pollster = "0.4"
bytemuck = { version = "1", features = ["derive"] }
env_logger = "0.11"
log = "0.4"
```

- [ ] **Step 3: Create module structure**

Create `src/capture/mod.rs`:
```rust
pub mod format;
pub mod video;
pub mod audio;
```

Create `src/render/mod.rs`:
```rust
pub mod display;
pub mod overlay;
```

Create `src/record/mod.rs`:
```rust
pub mod encoder;
pub mod screenshot;
```

Create placeholder files so the project compiles. Each of these files should contain only:
```rust
// Will be implemented in subsequent tasks
```

Files: `src/capture/video.rs`, `src/capture/audio.rs`, `src/render/display.rs`, `src/render/overlay.rs`, `src/record/encoder.rs`, `src/record/screenshot.rs`

- [ ] **Step 4: Write the failing test for CaptureFormat display**

Create `tests/format_test.rs`:
```rust
use genki_arcade::capture::format::{CaptureFormat, PixelFormat};

#[test]
fn test_capture_format_display_1080p60() {
    let fmt = CaptureFormat {
        width: 1920,
        height: 1080,
        fps: 60,
        pixel_format: PixelFormat::Mjpeg,
    };
    assert_eq!(fmt.to_string(), "1080p60");
}

#[test]
fn test_capture_format_display_1440p30() {
    let fmt = CaptureFormat {
        width: 2560,
        height: 1440,
        fps: 30,
        pixel_format: PixelFormat::Yuyv,
    };
    assert_eq!(fmt.to_string(), "1440p30");
}

#[test]
fn test_capture_format_display_720p60() {
    let fmt = CaptureFormat {
        width: 1280,
        height: 720,
        fps: 60,
        pixel_format: PixelFormat::Mjpeg,
    };
    assert_eq!(fmt.to_string(), "720p60");
}

#[test]
fn test_capture_format_display_non_standard() {
    let fmt = CaptureFormat {
        width: 1360,
        height: 768,
        fps: 60,
        pixel_format: PixelFormat::Mjpeg,
    };
    assert_eq!(fmt.to_string(), "1360x768@60");
}
```

- [ ] **Step 5: Run test to verify it fails**

Run: `cargo test --test format_test 2>&1 | head -30`
Expected: Compilation error — `CaptureFormat` not defined.

- [ ] **Step 6: Implement data types in format.rs**

Replace `src/capture/format.rs`:
```rust
use std::fmt;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Mjpeg,
    Yuyv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pixel_format: PixelFormat,
}

impl fmt::Display for CaptureFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self.height {
            480 if self.width == 640 || self.width == 720 => Some("480"),
            576 if self.width == 720 => Some("576"),
            600 if self.width == 800 => Some("600"),
            720 if self.width == 1280 => Some("720"),
            768 if self.width == 1024 => Some("768"),
            960 if self.width == 1280 => Some("960"),
            1024 if self.width == 1280 => Some("1024"),
            1080 if self.width == 1920 => Some("1080"),
            1440 if self.width == 2560 => Some("1440"),
            _ => None,
        };
        match label {
            Some(l) => write!(f, "{}p{}", l, self.fps),
            None => write!(f, "{}x{}@{}", self.width, self.height, self.fps),
        }
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub timestamp: Instant,
}
```

Update `src/main.rs` to expose modules as a library:

Create `src/lib.rs`:
```rust
pub mod capture;
pub mod render;
pub mod record;
```

Leave `src/main.rs` as:
```rust
fn main() {
    println!("genki-arcade");
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --test format_test -v`
Expected: All 4 tests pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: project scaffold with data types and format display"
```

---

## Task 2: YUYV to RGB Conversion

**Files:**
- Modify: `src/capture/format.rs`
- Test: `tests/format_test.rs`

- [ ] **Step 1: Write the failing test for YUYV->RGB conversion**

Append to `tests/format_test.rs`:
```rust
use genki_arcade::capture::format::yuyv_to_rgb;

#[test]
fn test_yuyv_to_rgb_single_macropixel() {
    // YUYV macropixel: Y0=128, U=128, Y1=128, V=128 -> neutral gray
    let yuyv = vec![128, 128, 128, 128];
    let rgb = yuyv_to_rgb(&yuyv, 2, 1);
    assert_eq!(rgb.len(), 6); // 2 pixels * 3 bytes
    // Neutral gray: Y=128, U=128, V=128 -> R≈128, G≈128, B≈128
    assert!((rgb[0] as i32 - 128).abs() < 3);
    assert!((rgb[1] as i32 - 128).abs() < 3);
    assert!((rgb[2] as i32 - 128).abs() < 3);
    assert!((rgb[3] as i32 - 128).abs() < 3);
    assert!((rgb[4] as i32 - 128).abs() < 3);
    assert!((rgb[5] as i32 - 128).abs() < 3);
}

#[test]
fn test_yuyv_to_rgb_black() {
    // Black: Y=16, U=128, V=128 (BT.601 studio range)
    let yuyv = vec![16, 128, 16, 128];
    let rgb = yuyv_to_rgb(&yuyv, 2, 1);
    // Should be near black
    assert!(rgb[0] < 10);
    assert!(rgb[1] < 10);
    assert!(rgb[2] < 10);
}

#[test]
fn test_yuyv_to_rgb_white() {
    // White: Y=235, U=128, V=128 (BT.601 studio range)
    let yuyv = vec![235, 128, 235, 128];
    let rgb = yuyv_to_rgb(&yuyv, 2, 1);
    // Should be near white
    assert!(rgb[0] > 245);
    assert!(rgb[1] > 245);
    assert!(rgb[2] > 245);
}

#[test]
fn test_yuyv_to_rgb_output_size() {
    // 4x2 image = 8 pixels = 16 bytes YUYV, 24 bytes RGB
    let yuyv = vec![128u8; 16];
    let rgb = yuyv_to_rgb(&yuyv, 4, 2);
    assert_eq!(rgb.len(), 24);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test format_test yuyv 2>&1 | head -20`
Expected: Compilation error — `yuyv_to_rgb` not defined.

- [ ] **Step 3: Implement YUYV->RGB conversion**

Add to `src/capture/format.rs`:
```rust
/// Convert YUYV (4:2:2) buffer to RGB24.
/// Uses BT.601 studio-range conversion.
/// Each 4 bytes of YUYV produces 2 RGB pixels.
pub fn yuyv_to_rgb(yuyv: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgb = Vec::with_capacity(pixel_count * 3);

    for chunk in yuyv.chunks_exact(4) {
        let y0 = chunk[0] as f32;
        let u = chunk[1] as f32;
        let y1 = chunk[2] as f32;
        let v = chunk[3] as f32;

        for y in [y0, y1] {
            let c = y - 16.0;
            let d = u - 128.0;
            let e = v - 128.0;

            let r = (1.164 * c + 1.596 * e).clamp(0.0, 255.0) as u8;
            let g = (1.164 * c - 0.392 * d - 0.813 * e).clamp(0.0, 255.0) as u8;
            let b = (1.164 * c + 2.017 * d).clamp(0.0, 255.0) as u8;

            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }

    rgb
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test format_test -v`
Expected: All 8 tests pass (4 display + 4 YUYV).

- [ ] **Step 5: Commit**

```bash
git add src/capture/format.rs tests/format_test.rs
git commit -m "feat: YUYV to RGB colorspace conversion"
```

---

## Task 3: MJPEG Decode

**Files:**
- Modify: `src/capture/format.rs`
- Test: `tests/format_test.rs`

- [ ] **Step 1: Write the failing test for MJPEG decode**

Append to `tests/format_test.rs`:
```rust
use genki_arcade::capture::format::mjpeg_to_rgb;

#[test]
fn test_mjpeg_to_rgb_valid_jpeg() {
    // Create a minimal valid JPEG using the image crate
    use image::{RgbImage, Rgb};
    use std::io::Cursor;

    let mut img = RgbImage::new(4, 4);
    for pixel in img.pixels_mut() {
        *pixel = Rgb([255, 0, 0]); // red
    }
    let mut jpeg_buf = Vec::new();
    let mut cursor = Cursor::new(&mut jpeg_buf);
    img.write_to(&mut cursor, image::ImageFormat::Jpeg).unwrap();

    let result = mjpeg_to_rgb(&jpeg_buf);
    assert!(result.is_ok());
    let (rgb, width, height) = result.unwrap();
    assert_eq!(width, 4);
    assert_eq!(height, 4);
    assert_eq!(rgb.len(), 4 * 4 * 3);
    // JPEG is lossy, so red channel should be close to 255
    assert!(rgb[0] > 200); // R
}

#[test]
fn test_mjpeg_to_rgb_invalid_data() {
    let garbage = vec![0u8; 100];
    let result = mjpeg_to_rgb(&garbage);
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test format_test mjpeg 2>&1 | head -20`
Expected: Compilation error — `mjpeg_to_rgb` not defined.

- [ ] **Step 3: Implement MJPEG decode**

Add to `src/capture/format.rs`:
```rust
use zune_jpeg::JpegDecoder;

/// Decode an MJPEG frame (JPEG buffer) to RGB24.
/// Returns (rgb_data, width, height).
pub fn mjpeg_to_rgb(jpeg_data: &[u8]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let mut decoder = JpegDecoder::new(jpeg_data);
    decoder.decode_headers().map_err(|e| anyhow::anyhow!("JPEG header error: {:?}", e))?;

    let (width, height) = decoder.dimensions().ok_or_else(|| anyhow::anyhow!("No dimensions in JPEG"))?;

    let pixels = decoder.decode().map_err(|e| anyhow::anyhow!("JPEG decode error: {:?}", e))?;

    Ok((pixels, width as u32, height as u32))
}
```

Add to the top of `src/capture/format.rs`:
```rust
use anyhow;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test format_test -v`
Expected: All 10 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/capture/format.rs tests/format_test.rs
git commit -m "feat: MJPEG to RGB decode via zune-jpeg"
```

---

## Task 4: VideoSource Trait + V4L2 Implementation

**Files:**
- Modify: `src/capture/video.rs`
- Modify: `src/capture/mod.rs`

- [ ] **Step 1: Implement the VideoSource trait and V4L2 backend**

Replace `src/capture/video.rs`:
```rust
use anyhow::{Context, Result};
use v4l::buffer::Type as BufType;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;
use v4l::FourCC;

use super::format::{CaptureFormat, Frame, PixelFormat, mjpeg_to_rgb, yuyv_to_rgb};

use std::time::Instant;

/// Trait for video capture sources. Enables mocking in tests.
pub trait VideoSource {
    fn supported_formats(&self) -> Vec<CaptureFormat>;
    fn set_format(&mut self, format: &CaptureFormat) -> Result<()>;
    fn start(&mut self) -> Result<()>;
    fn next_frame(&mut self) -> Result<Frame>;
    fn stop(&mut self) -> Result<()>;
}

pub struct V4l2Source {
    device: Device,
    stream: Option<Stream<'static>>,
    current_format: Option<CaptureFormat>,
}

impl V4l2Source {
    pub fn new(device_path: &str) -> Result<Self> {
        let device = Device::with_path(device_path)
            .with_context(|| format!("Failed to open V4L2 device: {}", device_path))?;
        Ok(Self {
            device,
            stream: None,
            current_format: None,
        })
    }
}

impl VideoSource for V4l2Source {
    fn supported_formats(&self) -> Vec<CaptureFormat> {
        let mut formats = Vec::new();

        let format_descs = match self.device.enum_formats() {
            Ok(f) => f,
            Err(_) => return formats,
        };

        for desc in &format_descs {
            let pixel_format = match desc.fourcc {
                FourCC { repr: [b'M', b'J', b'P', b'G'] } => PixelFormat::Mjpeg,
                FourCC { repr: [b'Y', b'U', b'Y', b'V'] } => PixelFormat::Yuyv,
                _ => continue,
            };

            let framesizes = match self.device.enum_framesizes(desc.fourcc) {
                Ok(f) => f,
                Err(_) => continue,
            };

            for framesize in &framesizes {
                match &framesize.size {
                    v4l::framesize::FrameSizeEnum::Discrete(d) => {
                        let frameintervals = match self.device.enum_frameintervals(
                            desc.fourcc,
                            d.width,
                            d.height,
                        ) {
                            Ok(f) => f,
                            Err(_) => continue,
                        };
                        for fi in &frameintervals {
                            match &fi.interval {
                                v4l::frameinterval::FrameIntervalEnum::Discrete(frac) => {
                                    let fps = frac.denominator / frac.numerator;
                                    formats.push(CaptureFormat {
                                        width: d.width,
                                        height: d.height,
                                        fps,
                                        pixel_format,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Sort: highest resolution first, then highest fps
        formats.sort_by(|a, b| {
            let res_a = a.width * a.height;
            let res_b = b.width * b.height;
            res_b.cmp(&res_a).then(b.fps.cmp(&a.fps))
        });

        formats
    }

    fn set_format(&mut self, format: &CaptureFormat) -> Result<()> {
        let fourcc = match format.pixel_format {
            PixelFormat::Mjpeg => FourCC::new(b"MJPG"),
            PixelFormat::Yuyv => FourCC::new(b"YUYV"),
        };

        let mut v4l_fmt = self.device.format().context("Failed to get current format")?;
        v4l_fmt.width = format.width;
        v4l_fmt.height = format.height;
        v4l_fmt.fourcc = fourcc;
        self.device.set_format(&v4l_fmt).context("Failed to set format")?;

        // Set frame interval (fps)
        let mut params = self.device.params().context("Failed to get params")?;
        params.interval = v4l::Fraction::new(1, format.fps);
        self.device.set_params(&params).context("Failed to set params")?;

        self.current_format = Some(format.clone());
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        // We need an owned stream. Use unsafe to extend the lifetime.
        // This is safe because V4l2Source owns the Device and we ensure
        // the stream is dropped before the device.
        let device_ptr = &self.device as *const Device;
        let device_ref: &'static Device = unsafe { &*device_ptr };
        let stream = Stream::with_buffers(device_ref, BufType::VideoCapture, 4)
            .context("Failed to create mmap stream")?;
        self.stream = Some(stream);
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Frame> {
        let stream = self.stream.as_mut().context("Stream not started")?;
        let (buf, _meta) = stream.next().context("Failed to get next frame")?;
        let format = self.current_format.as_ref().context("Format not set")?;

        let (rgb_data, width, height) = match format.pixel_format {
            PixelFormat::Mjpeg => mjpeg_to_rgb(buf)?,
            PixelFormat::Yuyv => {
                let rgb = yuyv_to_rgb(buf, format.width, format.height);
                (rgb, format.width, format.height)
            }
        };

        Ok(Frame {
            width,
            height,
            data: rgb_data,
            timestamp: Instant::now(),
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.stream = None;
        Ok(())
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: No errors (warnings are OK).

- [ ] **Step 3: Commit**

```bash
git add src/capture/video.rs
git commit -m "feat: VideoSource trait and V4L2 capture implementation"
```

---

## Task 5: AudioSource Trait + cpal Implementation

**Files:**
- Modify: `src/capture/audio.rs`
- Test: `tests/audio_test.rs`

- [ ] **Step 1: Write the failing test for volume scaling**

Create `tests/audio_test.rs`:
```rust
use genki_arcade::capture::audio::scale_volume;

#[test]
fn test_scale_volume_full() {
    let samples = vec![1000i16, -1000, 500, -500];
    let scaled = scale_volume(&samples, 1.0);
    assert_eq!(scaled, vec![1000, -1000, 500, -500]);
}

#[test]
fn test_scale_volume_half() {
    let samples = vec![1000i16, -1000, 500, -500];
    let scaled = scale_volume(&samples, 0.5);
    assert_eq!(scaled, vec![500, -500, 250, -250]);
}

#[test]
fn test_scale_volume_mute() {
    let samples = vec![1000i16, -1000, 32767, -32768];
    let scaled = scale_volume(&samples, 0.0);
    assert_eq!(scaled, vec![0, 0, 0, 0]);
}

#[test]
fn test_scale_volume_clipping() {
    let samples = vec![32767i16, -32768];
    let scaled = scale_volume(&samples, 1.0);
    assert_eq!(scaled[0], 32767);
    assert_eq!(scaled[1], -32768);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test audio_test 2>&1 | head -20`
Expected: Compilation error — `scale_volume` not defined.

- [ ] **Step 3: Implement AudioSource**

Replace `src/capture/audio.rs`:
```rust
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender};
use ringbuf::{HeapRb, traits::{Producer, Consumer, Split}};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Scale audio samples by a volume factor (0.0 to 1.0).
pub fn scale_volume(samples: &[i16], volume: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let scaled = (s as f32 * volume).round();
            scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect()
}

/// Trait for audio capture sources. Enables mocking in tests.
pub trait AudioSource {
    fn start(&mut self) -> Result<()>;
    fn set_volume(&self, volume: f32);
    fn stop(&mut self);
}

/// Provides a channel receiver for audio samples (used by the encoder).
pub trait AudioSampleReceiver {
    fn audio_receiver(&self) -> Option<Receiver<Vec<i16>>>;
}

pub struct CpalAudioSource {
    device_name: String,
    input_stream: Option<Stream>,
    output_stream: Option<Stream>,
    volume: Arc<AtomicU32>,
    recording: Arc<AtomicBool>,
    audio_tx: Sender<Vec<i16>>,
    audio_rx: Receiver<Vec<i16>>,
}

impl CpalAudioSource {
    pub fn new(device_name: &str) -> Self {
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(64);
        Self {
            device_name: device_name.to_string(),
            input_stream: None,
            output_stream: None,
            volume: Arc::new(AtomicU32::new(f32::to_bits(1.0))),
            recording: Arc::new(AtomicBool::new(false)),
            audio_tx,
            audio_rx,
        }
    }

    pub fn set_recording(&self, recording: bool) {
        self.recording.store(recording, Ordering::Relaxed);
    }
}

impl AudioSource for CpalAudioSource {
    fn start(&mut self) -> Result<()> {
        let host = cpal::default_host();

        // Find the Shadowcast 2 input device
        let input_device = host
            .input_devices()
            .context("Failed to enumerate input devices")?
            .find(|d| {
                d.name()
                    .map(|n| n.contains(&self.device_name))
                    .unwrap_or(false)
            })
            .context("Shadowcast 2 audio device not found")?;

        let config = StreamConfig {
            channels: 2,
            sample_rate: SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        // Ring buffer for input -> output (live playback)
        let rb = HeapRb::<i16>::new(48000); // ~500ms buffer
        let (mut producer, mut consumer) = rb.split();

        let volume = self.volume.clone();
        let recording = self.recording.clone();
        let tx = self.audio_tx.clone();

        let input_stream = input_device
            .build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                    let scaled = scale_volume(data, vol);

                    // Push to ring buffer for live playback
                    for &sample in &scaled {
                        let _ = producer.try_push(sample);
                    }

                    // Send to encoder if recording
                    if recording.load(Ordering::Relaxed) {
                        let _ = tx.try_send(scaled);
                    }
                },
                |err| log::error!("Audio input error: {}", err),
                None,
            )
            .context("Failed to build input stream")?;

        // Output stream for live playback
        let output_device = host
            .default_output_device()
            .context("No output device found")?;

        let output_stream = output_device
            .build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() {
                        *sample = consumer.try_pop().unwrap_or(0);
                    }
                },
                |err| log::error!("Audio output error: {}", err),
                None,
            )
            .context("Failed to build output stream")?;

        input_stream.play().context("Failed to play input stream")?;
        output_stream.play().context("Failed to play output stream")?;

        self.input_stream = Some(input_stream);
        self.output_stream = Some(output_stream);

        Ok(())
    }

    fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume.store(clamped.to_bits(), Ordering::Relaxed);
    }

    fn stop(&mut self) {
        self.input_stream = None;
        self.output_stream = None;
    }
}

impl AudioSampleReceiver for CpalAudioSource {
    fn audio_receiver(&self) -> Option<Receiver<Vec<i16>>> {
        Some(self.audio_rx.clone())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test audio_test -v`
Expected: All 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/capture/audio.rs tests/audio_test.rs
git commit -m "feat: AudioSource trait with cpal capture and volume scaling"
```

---

## Task 6: wgpu Display Pipeline + Shader

**Files:**
- Modify: `src/render/display.rs`
- Create: `src/render/shader.wgsl`

- [ ] **Step 1: Write the WGSL shader**

Create `src/render/shader.wgsl`:
```wgsl
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle (covers the screen with 3 vertices)
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );

    var tex_coords = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );

    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.tex_coords = tex_coords[vertex_index];
    return output;
}

@group(0) @binding(0) var video_texture: texture_2d<f32>;
@group(0) @binding(1) var video_sampler: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(video_texture, video_sampler, in.tex_coords);
}
```

- [ ] **Step 2: Implement the display renderer**

Replace `src/render/display.rs`:
```rust
use anyhow::{Context, Result};
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

pub struct DisplayRenderer {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub surface: wgpu::Surface<'static>,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub render_pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    pub texture: Option<wgpu::Texture>,
    pub texture_view: Option<wgpu::TextureView>,
    pub bind_group: Option<wgpu::BindGroup>,
    pub texture_width: u32,
    pub texture_height: u32,
}

impl DisplayRenderer {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .context("Failed to create surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("Failed to find GPU adapter")?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("genki-arcade device"),
                ..Default::default()
            })
            .await
            .context("Failed to create device")?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let size = window.inner_size();
        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("video bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Ok(Self {
            device,
            queue,
            surface,
            surface_config,
            render_pipeline,
            bind_group_layout,
            sampler,
            texture: None,
            texture_view: None,
            bind_group: None,
            texture_width: 0,
            texture_height: 0,
        })
    }

    /// Upload an RGB24 frame to the GPU texture.
    /// Recreates the texture if dimensions changed.
    pub fn upload_frame(&mut self, rgb_data: &[u8], width: u32, height: u32) {
        if self.texture_width != width || self.texture_height != height {
            self.create_texture(width, height);
        }

        if let Some(texture) = &self.texture {
            // Convert RGB24 to RGBA32 (wgpu needs 4-byte alignment)
            let rgba: Vec<u8> = rgb_data
                .chunks_exact(3)
                .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                .collect();

            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn create_texture(&mut self, width: u32, height: u32) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let texture_view = texture.create_view(&Default::default());

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        self.texture = Some(texture);
        self.texture_view = Some(texture_view);
        self.bind_group = Some(bind_group);
        self.texture_width = width;
        self.texture_height = height;
    }

    /// Render the video quad. Returns the encoder for egui to append to.
    pub fn render_frame(&self) -> Result<(wgpu::SurfaceTexture, wgpu::CommandEncoder)> {
        let output = self
            .surface
            .get_current_texture()
            .context("Failed to get surface texture")?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        if let Some(bind_group) = &self.bind_group {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        Ok((output, encoder))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.device, &self.surface_config);
        }
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_config.format
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: No errors.

- [ ] **Step 4: Commit**

```bash
git add src/render/display.rs src/render/shader.wgsl
git commit -m "feat: wgpu display renderer with textured quad pipeline"
```

---

## Task 7: egui Toolbar Overlay

**Files:**
- Modify: `src/render/overlay.rs`

- [ ] **Step 1: Implement the toolbar overlay**

Replace `src/render/overlay.rs`:
```rust
use crate::capture::format::CaptureFormat;

use std::time::{Duration, Instant};

pub struct Toolbar {
    pub visible: bool,
    pub volume: f32,
    pub selected_format_index: usize,
    pub is_recording: bool,
    pub recording_start: Option<Instant>,
    last_mouse_over: Option<Instant>,
    auto_hide_delay: Duration,

    // Action flags — read and cleared by the app each frame
    pub screenshot_requested: bool,
    pub recording_toggled: bool,
    pub format_changed: bool,
}

impl Toolbar {
    pub fn new() -> Self {
        Self {
            visible: false,
            volume: 1.0,
            selected_format_index: 0,
            is_recording: false,
            recording_start: None,
            last_mouse_over: None,
            auto_hide_delay: Duration::from_secs(3),
            screenshot_requested: false,
            recording_toggled: false,
            format_changed: false,
        }
    }

    pub fn toggle_visible(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.last_mouse_over = Some(Instant::now());
        }
    }

    pub fn toggle_recording(&mut self) {
        self.is_recording = !self.is_recording;
        if self.is_recording {
            self.recording_start = Some(Instant::now());
        } else {
            self.recording_start = None;
        }
        self.recording_toggled = true;
    }

    pub fn recording_elapsed(&self) -> Duration {
        self.recording_start
            .map(|start| start.elapsed())
            .unwrap_or_default()
    }

    /// Draw the toolbar. Call this inside egui's frame closure.
    pub fn ui(&mut self, ctx: &egui::Context, formats: &[CaptureFormat]) {
        // Check auto-hide
        if self.visible {
            if let Some(last) = self.last_mouse_over {
                if last.elapsed() > self.auto_hide_delay && !self.is_recording {
                    self.visible = false;
                }
            }
        }

        let screen_rect = ctx.screen_rect();

        // Always show the toggle pill button
        let pill_width = 80.0;
        let pill_height = 24.0;
        let pill_x = screen_rect.center().x - pill_width / 2.0;
        let pill_y = screen_rect.max.y - if self.visible { 52.0 } else { 32.0 };

        egui::Area::new(egui::Id::new("toolbar_toggle"))
            .fixed_pos(egui::pos2(pill_x, pill_y))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let btn = ui.add(
                    egui::Button::new(if self.visible { "▼ Hide" } else { "▲ Controls" })
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30))
                        .corner_radius(12.0),
                );
                if btn.clicked() {
                    self.toggle_visible();
                }
            });

        if !self.visible {
            return;
        }

        // Toolbar panel at bottom
        egui::TopBottomPanel::bottom("toolbar")
            .frame(egui::Frame::new().fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190)))
            .show(ctx, |ui| {
                // Reset auto-hide timer on hover
                if ui.rect_contains_pointer(ui.max_rect()) {
                    self.last_mouse_over = Some(Instant::now());
                }

                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;

                    // Volume control
                    ui.label("🔊");
                    let vol_response = ui.add(
                        egui::Slider::new(&mut self.volume, 0.0..=1.0)
                            .show_value(false)
                            .custom_formatter(|v, _| format!("{}%", (v * 100.0) as u32)),
                    );
                    if vol_response.changed() {
                        self.last_mouse_over = Some(Instant::now());
                    }

                    ui.separator();

                    // Resolution dropdown
                    if !formats.is_empty() {
                        let current_label = formats
                            .get(self.selected_format_index)
                            .map(|f| f.to_string())
                            .unwrap_or_else(|| "---".to_string());

                        let old_index = self.selected_format_index;
                        egui::ComboBox::from_id_salt("resolution")
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                for (i, fmt) in formats.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.selected_format_index,
                                        i,
                                        fmt.to_string(),
                                    );
                                }
                            });
                        if self.selected_format_index != old_index {
                            self.format_changed = true;
                            self.last_mouse_over = Some(Instant::now());
                        }
                    }

                    ui.separator();

                    // Record button
                    let record_label = if self.is_recording { "⏹ Stop" } else { "⏺ Rec" };
                    let record_color = if self.is_recording {
                        egui::Color32::from_rgb(255, 68, 68)
                    } else {
                        egui::Color32::from_rgb(200, 200, 200)
                    };
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new(record_label).color(record_color),
                        ))
                        .clicked()
                    {
                        self.toggle_recording();
                    }

                    // Screenshot button
                    if ui.button("📸").clicked() {
                        self.screenshot_requested = true;
                    }

                    // Recording timer
                    if self.is_recording {
                        let elapsed = self.recording_elapsed();
                        let secs = elapsed.as_secs();
                        let h = secs / 3600;
                        let m = (secs % 3600) / 60;
                        let s = secs % 60;
                        ui.label(
                            egui::RichText::new(format!("{:02}:{:02}:{:02}", h, m, s))
                                .color(egui::Color32::from_rgb(150, 150, 150))
                                .small(),
                        );
                    }
                });
            });

        // Request repaint for timer updates and auto-hide
        ctx.request_repaint();
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: No errors.

- [ ] **Step 3: Commit**

```bash
git add src/render/overlay.rs
git commit -m "feat: egui toolbar overlay with volume, resolution, record, screenshot"
```

---

## Task 8: Screenshot Capture

**Files:**
- Modify: `src/record/screenshot.rs`
- Test: `tests/screenshot_test.rs`

- [ ] **Step 1: Write the failing test for screenshot path generation**

Create `tests/screenshot_test.rs`:
```rust
use genki_arcade::record::screenshot::screenshot_path;

#[test]
fn test_screenshot_path_format() {
    let path = screenshot_path();
    let filename = path.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("screenshot-"));
    assert!(filename.ends_with(".png"));
    assert!(path.parent().unwrap().ends_with("genki-arcade"));
}

#[test]
fn test_screenshot_paths_are_unique() {
    let path1 = screenshot_path();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let path2 = screenshot_path();
    assert_ne!(path1, path2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test screenshot_test 2>&1 | head -20`
Expected: Compilation error — `screenshot_path` not defined.

- [ ] **Step 3: Implement screenshot module**

Replace `src/record/screenshot.rs`:
```rust
use anyhow::{Context, Result};
use chrono::Local;
use directories::UserDirs;
use image::{ImageBuffer, Rgb};
use std::path::PathBuf;

/// Generate the output path for a screenshot.
pub fn screenshot_path() -> PathBuf {
    let base = UserDirs::new()
        .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = base.join("genki-arcade");
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    dir.join(format!("screenshot-{}.png", timestamp))
}

/// Save an RGB24 buffer as a PNG screenshot. Runs on a spawned thread.
pub fn take_screenshot(rgb_data: Vec<u8>, width: u32, height: u32) {
    std::thread::spawn(move || {
        if let Err(e) = save_png(&rgb_data, width, height) {
            log::error!("Screenshot failed: {}", e);
        }
    });
}

fn save_png(rgb_data: &[u8], width: u32, height: u32) -> Result<()> {
    let path = screenshot_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {:?}", parent))?;
    }

    let img: ImageBuffer<Rgb<u8>, _> =
        ImageBuffer::from_raw(width, height, rgb_data.to_vec())
            .context("Failed to create image buffer from RGB data")?;

    img.save(&path)
        .with_context(|| format!("Failed to save screenshot to {:?}", path))?;

    log::info!("Screenshot saved to {:?}", path);
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test screenshot_test -v`
Expected: All 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/record/screenshot.rs tests/screenshot_test.rs
git commit -m "feat: PNG screenshot capture with XDG path support"
```

---

## Task 9: Encoder Trait + ffmpeg-next Implementation

**Files:**
- Modify: `src/record/encoder.rs`
- Test: `tests/encoder_test.rs`

- [ ] **Step 1: Write the failing test for recording path generation and mock encoder**

Create `tests/encoder_test.rs`:
```rust
use genki_arcade::record::encoder::{recording_path, Encoder, EncoderConfig};

#[test]
fn test_recording_path_format() {
    let path = recording_path();
    let filename = path.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("recording-"));
    assert!(filename.ends_with(".mp4"));
    assert!(path.parent().unwrap().ends_with("genki-arcade"));
}

#[test]
fn test_encoder_config_defaults() {
    let config = EncoderConfig {
        width: 1920,
        height: 1080,
        fps: 60,
        audio_sample_rate: 48000,
        audio_channels: 2,
    };
    assert_eq!(config.width, 1920);
    assert_eq!(config.fps, 60);
    assert_eq!(config.audio_sample_rate, 48000);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test encoder_test 2>&1 | head -20`
Expected: Compilation error — types not defined.

- [ ] **Step 3: Implement Encoder trait and ffmpeg backend**

Replace `src/record/encoder.rs`:
```rust
use anyhow::{Context, Result};
use chrono::Local;
use crossbeam_channel::Receiver;
use directories::UserDirs;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

/// Generate the output path for a recording.
pub fn recording_path() -> PathBuf {
    let base = UserDirs::new()
        .and_then(|d| d.video_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = base.join("genki-arcade");
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    dir.join(format!("recording-{}.mp4", timestamp))
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
}

/// Trait for video/audio encoding. Enables mocking in tests.
pub trait Encoder: Send {
    fn start(
        &mut self,
        config: EncoderConfig,
        video_rx: Receiver<Vec<u8>>,
        audio_rx: Receiver<Vec<i16>>,
    ) -> Result<()>;
    fn stop(&mut self) -> Result<PathBuf>;
    fn is_running(&self) -> bool;
}

pub struct FfmpegEncoder {
    thread: Option<JoinHandle<Result<PathBuf>>>,
    stop_tx: Option<crossbeam_channel::Sender<()>>,
}

impl FfmpegEncoder {
    pub fn new() -> Self {
        Self {
            thread: None,
            stop_tx: None,
        }
    }
}

impl Encoder for FfmpegEncoder {
    fn start(
        &mut self,
        config: EncoderConfig,
        video_rx: Receiver<Vec<u8>>,
        audio_rx: Receiver<Vec<i16>>,
    ) -> Result<()> {
        let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
        self.stop_tx = Some(stop_tx);

        let output_path = recording_path();
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let handle = thread::spawn(move || -> Result<PathBuf> {
            encode_loop(config, video_rx, audio_rx, stop_rx, &output_path)?;
            Ok(output_path)
        });

        self.thread = Some(handle);
        Ok(())
    }

    fn stop(&mut self) -> Result<PathBuf> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.thread.take() {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("Encoder thread panicked"))?
        } else {
            Err(anyhow::anyhow!("Encoder not running"))
        }
    }

    fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

fn encode_loop(
    config: EncoderConfig,
    video_rx: Receiver<Vec<u8>>,
    audio_rx: Receiver<Vec<i16>>,
    stop_rx: Receiver<()>,
    output_path: &PathBuf,
) -> Result<()> {
    ffmpeg_next::init().context("Failed to init ffmpeg")?;

    let mut octx =
        ffmpeg_next::format::output(output_path).context("Failed to create output context")?;

    // Video stream
    let video_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264)
        .context("H.264 encoder not found")?;
    let mut video_stream = octx.add_stream(video_codec).context("Failed to add video stream")?;

    let video_stream_index = video_stream.index();

    let mut video_encoder = ffmpeg_next::codec::context::Context::from_parameters(
        video_stream.parameters(),
    )?
    .encoder()
    .video()?;

    video_encoder.set_width(config.width);
    video_encoder.set_height(config.height);
    video_encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
    video_encoder.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
    video_encoder.set_bit_rate(8_000_000); // 8 Mbps

    let mut video_encoder = video_encoder
        .open_as(video_codec)
        .context("Failed to open video encoder")?;

    video_stream.set_parameters(&video_encoder);

    // Audio stream
    let audio_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::AAC)
        .context("AAC encoder not found")?;
    let mut audio_stream = octx.add_stream(audio_codec).context("Failed to add audio stream")?;

    let audio_stream_index = audio_stream.index();

    let mut audio_encoder = ffmpeg_next::codec::context::Context::from_parameters(
        audio_stream.parameters(),
    )?
    .encoder()
    .audio()?;

    audio_encoder.set_rate(config.audio_sample_rate as i32);
    audio_encoder.set_channels(config.audio_channels as i32);
    audio_encoder.set_format(ffmpeg_next::format::Sample::I16(
        ffmpeg_next::format::sample::Type::Packed,
    ));
    audio_encoder.set_time_base(ffmpeg_next::Rational::new(1, config.audio_sample_rate as i32));

    let mut audio_encoder = audio_encoder
        .open_as(audio_codec)
        .context("Failed to open audio encoder")?;

    audio_stream.set_parameters(&audio_encoder);

    octx.write_header().context("Failed to write header")?;

    let mut video_pts: i64 = 0;
    let mut audio_pts: i64 = 0;

    loop {
        // Check for stop signal
        if stop_rx.try_recv().is_ok() {
            break;
        }

        // Process video frames
        if let Ok(rgb_data) = video_rx.try_recv() {
            let mut frame = ffmpeg_next::frame::Video::new(
                ffmpeg_next::format::Pixel::YUV420P,
                config.width,
                config.height,
            );

            // Convert RGB to YUV420P using ffmpeg's scaler
            let mut rgb_frame = ffmpeg_next::frame::Video::new(
                ffmpeg_next::format::Pixel::RGB24,
                config.width,
                config.height,
            );
            rgb_frame.data_mut(0)[..rgb_data.len()].copy_from_slice(&rgb_data);

            let mut scaler = ffmpeg_next::software::scaling::Context::get(
                ffmpeg_next::format::Pixel::RGB24,
                config.width,
                config.height,
                ffmpeg_next::format::Pixel::YUV420P,
                config.width,
                config.height,
                ffmpeg_next::software::scaling::Flags::BILINEAR,
            )?;
            scaler.run(&rgb_frame, &mut frame)?;

            frame.set_pts(Some(video_pts));
            video_pts += 1;

            video_encoder.send_frame(&frame)?;

            let mut packet = ffmpeg_next::Packet::empty();
            while video_encoder.receive_packet(&mut packet).is_ok() {
                packet.set_stream(video_stream_index);
                packet.rescale_ts(
                    video_encoder.time_base(),
                    octx.stream(video_stream_index).unwrap().time_base(),
                );
                packet.write_interleaved(&mut octx)?;
            }
        }

        // Process audio samples
        if let Ok(samples) = audio_rx.try_recv() {
            let frame_size = audio_encoder.frame_size() as usize;
            if frame_size > 0 && samples.len() >= frame_size * config.audio_channels as usize {
                let mut frame = ffmpeg_next::frame::Audio::new(
                    ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
                    frame_size,
                    ffmpeg_next::ChannelLayout::STEREO,
                );

                let data_bytes: Vec<u8> = samples
                    .iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect();
                frame.data_mut(0)[..data_bytes.len()].copy_from_slice(&data_bytes);

                frame.set_pts(Some(audio_pts));
                audio_pts += frame_size as i64;

                audio_encoder.send_frame(&frame)?;

                let mut packet = ffmpeg_next::Packet::empty();
                while audio_encoder.receive_packet(&mut packet).is_ok() {
                    packet.set_stream(audio_stream_index);
                    packet.rescale_ts(
                        audio_encoder.time_base(),
                        octx.stream(audio_stream_index).unwrap().time_base(),
                    );
                    packet.write_interleaved(&mut octx)?;
                }
            }
        }

        // Small sleep to avoid busy-waiting
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // Flush video encoder
    video_encoder.send_eof()?;
    let mut packet = ffmpeg_next::Packet::empty();
    while video_encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(video_stream_index);
        packet.rescale_ts(
            video_encoder.time_base(),
            octx.stream(video_stream_index).unwrap().time_base(),
        );
        packet.write_interleaved(&mut octx)?;
    }

    // Flush audio encoder
    audio_encoder.send_eof()?;
    while audio_encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(audio_stream_index);
        packet.rescale_ts(
            audio_encoder.time_base(),
            octx.stream(audio_stream_index).unwrap().time_base(),
        );
        packet.write_interleaved(&mut octx)?;
    }

    octx.write_trailer().context("Failed to write trailer")?;

    log::info!("Recording saved to {:?}", output_path);
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test encoder_test -v`
Expected: Both tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/record/encoder.rs tests/encoder_test.rs
git commit -m "feat: Encoder trait with ffmpeg H.264+AAC MP4 implementation"
```

---

## Task 10: Application Shell (main.rs + app.rs)

**Files:**
- Create: `src/app.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create the App struct with ApplicationHandler**

Create `src/app.rs`:
```rust
use crate::capture::audio::{AudioSampleReceiver, AudioSource, CpalAudioSource};
use crate::capture::format::CaptureFormat;
use crate::capture::video::{V4l2Source, VideoSource};
use crate::record::encoder::{Encoder, EncoderConfig, FfmpegEncoder};
use crate::record::screenshot;
use crate::render::display::DisplayRenderer;
use crate::render::overlay::Toolbar;

use anyhow::Result;
use crossbeam_channel::{bounded, Sender};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<DisplayRenderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    egui_ctx: egui::Context,
    video_source: Option<V4l2Source>,
    audio_source: Option<CpalAudioSource>,
    encoder: Option<FfmpegEncoder>,
    video_frame_tx: Option<Sender<Vec<u8>>>,
    toolbar: Toolbar,
    formats: Vec<CaptureFormat>,
    modifiers: ModifiersState,
    last_frame_rgb: Option<Vec<u8>>,
    last_frame_width: u32,
    last_frame_height: u32,
}

impl App {
    pub fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            egui_state: None,
            egui_renderer: None,
            egui_ctx: egui::Context::default(),
            video_source: None,
            audio_source: None,
            encoder: None,
            video_frame_tx: None,
            toolbar: Toolbar::new(),
            formats: Vec::new(),
            modifiers: ModifiersState::empty(),
            last_frame_rgb: None,
            last_frame_width: 0,
            last_frame_height: 0,
        }
    }

    fn init_capture(&mut self) -> Result<()> {
        let mut video = V4l2Source::new("/dev/video2")?;
        self.formats = video.supported_formats();

        // Default to first MJPEG 1080p60 if available, otherwise first format
        let default_index = self
            .formats
            .iter()
            .position(|f| {
                f.width == 1920
                    && f.height == 1080
                    && f.fps == 60
                    && f.pixel_format == crate::capture::format::PixelFormat::Mjpeg
            })
            .unwrap_or(0);

        if let Some(format) = self.formats.get(default_index) {
            video.set_format(format)?;
            self.toolbar.selected_format_index = default_index;
        }

        video.start()?;
        self.video_source = Some(video);

        // Audio
        let mut audio = CpalAudioSource::new("ShadowCast");
        if let Err(e) = audio.start() {
            log::warn!("Audio init failed (continuing video-only): {}", e);
        } else {
            self.audio_source = Some(audio);
        }

        Ok(())
    }

    fn handle_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }

        let ctrl = self.modifiers.control_key();

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => event_loop.exit(),
            Key::Named(NamedKey::F11) => {
                if let Some(window) = &self.window {
                    let is_fullscreen = window.fullscreen().is_some();
                    if is_fullscreen {
                        window.set_fullscreen(None);
                    } else {
                        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
                    }
                }
            }
            Key::Character(c) if ctrl && c.as_str() == "s" => {
                self.take_screenshot();
            }
            Key::Character(c) if ctrl && c.as_str() == "r" => {
                self.toolbar.toggle_recording();
                self.handle_recording_toggle();
            }
            _ => {}
        }
    }

    fn take_screenshot(&self) {
        if let Some(rgb) = &self.last_frame_rgb {
            screenshot::take_screenshot(rgb.clone(), self.last_frame_width, self.last_frame_height);
        }
    }

    fn handle_recording_toggle(&mut self) {
        if self.toolbar.is_recording {
            self.start_recording();
        } else {
            self.stop_recording();
        }
    }

    fn start_recording(&mut self) {
        let format = match self.formats.get(self.toolbar.selected_format_index) {
            Some(f) => f,
            None => return,
        };

        let config = EncoderConfig {
            width: format.width,
            height: format.height,
            fps: format.fps,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };

        let (video_tx, video_rx) = bounded(4);
        let audio_rx = self
            .audio_source
            .as_ref()
            .and_then(|a| a.audio_receiver())
            .unwrap_or_else(|| {
                let (_tx, rx) = bounded(1);
                rx
            });

        let mut encoder = FfmpegEncoder::new();
        if let Err(e) = encoder.start(config, video_rx, audio_rx) {
            log::error!("Failed to start recording: {}", e);
            self.toolbar.is_recording = false;
            self.toolbar.recording_start = None;
            return;
        }

        if let Some(audio) = &self.audio_source {
            audio.set_recording(true);
        }

        self.video_frame_tx = Some(video_tx);
        self.encoder = Some(encoder);
    }

    fn stop_recording(&mut self) {
        if let Some(audio) = &self.audio_source {
            audio.set_recording(false);
        }

        self.video_frame_tx = None;

        if let Some(mut encoder) = self.encoder.take() {
            match encoder.stop() {
                Ok(path) => log::info!("Recording saved: {:?}", path),
                Err(e) => log::error!("Failed to stop recording: {}", e),
            }
        }
    }

    fn handle_format_change(&mut self) {
        let new_format = match self.formats.get(self.toolbar.selected_format_index) {
            Some(f) => f.clone(),
            None => return,
        };

        if let Some(video) = &mut self.video_source {
            if let Err(e) = video.stop() {
                log::error!("Failed to stop stream: {}", e);
            }
            if let Err(e) = video.set_format(&new_format) {
                log::error!("Failed to set format: {}", e);
                return;
            }
            if let Err(e) = video.start() {
                log::error!("Failed to restart stream: {}", e);
                return;
            }
        }

        // Resize window
        if let Some(window) = &self.window {
            let _ = window.request_inner_size(LogicalSize::new(
                new_format.width as f64,
                new_format.height as f64,
            ));
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("genki-arcade")
            .with_inner_size(LogicalSize::new(1920.0, 1080.0))
            .with_visible(false);

        let window = Arc::new(event_loop.create_window(attrs).expect("Failed to create window"));

        let renderer = pollster::block_on(DisplayRenderer::new(window.clone()))
            .expect("Failed to create renderer");

        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
        );

        let egui_renderer = egui_wgpu::Renderer::new(
            &renderer.device,
            renderer.surface_format(),
            None,
            1,
            false,
        );

        self.renderer = Some(renderer);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
        self.window = Some(window.clone());

        if let Err(e) = self.init_capture() {
            log::error!("Failed to init capture: {}", e);
        }

        window.set_visible(true);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Let egui handle events first
        if let Some(state) = &mut self.egui_state {
            let response = state.on_window_event(&self.window.as_ref().unwrap(), &event);
            if response.consumed {
                return;
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                self.stop_recording();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(&event, event_loop);
            }
            WindowEvent::RedrawRequested => {
                // Capture a frame
                if let Some(video) = &mut self.video_source {
                    match video.next_frame() {
                        Ok(frame) => {
                            self.last_frame_rgb = Some(frame.data.clone());
                            self.last_frame_width = frame.width;
                            self.last_frame_height = frame.height;

                            // Send to encoder if recording
                            if let Some(tx) = &self.video_frame_tx {
                                let _ = tx.try_send(frame.data.clone());
                            }

                            if let Some(renderer) = &mut self.renderer {
                                renderer.upload_frame(&frame.data, frame.width, frame.height);
                            }
                        }
                        Err(e) => {
                            log::warn!("Frame capture failed: {}", e);
                        }
                    }
                }

                // Check toolbar actions
                if self.toolbar.format_changed {
                    self.toolbar.format_changed = false;
                    self.handle_format_change();
                }
                if self.toolbar.screenshot_requested {
                    self.toolbar.screenshot_requested = false;
                    self.take_screenshot();
                }
                if self.toolbar.recording_toggled {
                    self.toolbar.recording_toggled = false;
                    self.handle_recording_toggle();
                }

                // Update audio volume
                if let Some(audio) = &self.audio_source {
                    audio.set_volume(self.toolbar.volume);
                }

                // Render
                if let Some(renderer) = &self.renderer {
                    match renderer.render_frame() {
                        Ok((output, mut encoder)) => {
                            let window = self.window.as_ref().unwrap();
                            let egui_state = self.egui_state.as_mut().unwrap();
                            let egui_renderer = self.egui_renderer.as_mut().unwrap();

                            let raw_input = egui_state.take_egui_input(window);
                            let full_output = self.egui_ctx.run(raw_input, |ctx| {
                                self.toolbar.ui(ctx, &self.formats);
                            });

                            egui_state.handle_platform_output(window, full_output.platform_output);

                            let clipped = self
                                .egui_ctx
                                .tessellate(full_output.shapes, full_output.pixels_per_point);

                            let screen_desc = egui_wgpu::ScreenDescriptor {
                                size_in_pixels: [
                                    renderer.surface_config.width,
                                    renderer.surface_config.height,
                                ],
                                pixels_per_point: full_output.pixels_per_point,
                            };

                            for (id, delta) in &full_output.textures_delta.set {
                                egui_renderer.update_buffers(
                                    &renderer.device,
                                    &renderer.queue,
                                    &mut encoder,
                                    &clipped,
                                    &screen_desc,
                                );
                            }

                            // Render egui on top
                            let view = output.texture.create_view(&Default::default());
                            {
                                let mut render_pass =
                                    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                        label: Some("egui pass"),
                                        color_attachments: &[Some(
                                            wgpu::RenderPassColorAttachment {
                                                view: &view,
                                                resolve_target: None,
                                                ops: wgpu::Operations {
                                                    load: wgpu::LoadOp::Load,
                                                    store: wgpu::StoreOp::Store,
                                                },
                                            },
                                        )],
                                        depth_stencil_attachment: None,
                                        timestamp_writes: None,
                                        occlusion_query_set: None,
                                    });
                                egui_renderer.render(&mut render_pass, &clipped, &screen_desc);
                            }

                            // Free egui textures
                            for id in &full_output.textures_delta.free {
                                egui_renderer.free_texture(id);
                            }

                            renderer.queue.submit(std::iter::once(encoder.finish()));
                            output.present();
                        }
                        Err(e) => {
                            log::error!("Render failed: {}", e);
                        }
                    }
                }

                // Request next frame
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 2: Update src/lib.rs**

Replace `src/lib.rs`:
```rust
pub mod capture;
pub mod render;
pub mod record;
pub mod app;
```

- [ ] **Step 3: Write main.rs**

Replace `src/main.rs`:
```rust
use genki_arcade::app::App;
use winit::event_loop::EventLoop;

fn main() {
    env_logger::init();

    let event_loop = EventLoop::new().expect("Failed to create event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("Event loop error");
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: No errors (warnings OK). If there are minor API issues, fix them.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/main.rs src/lib.rs
git commit -m "feat: application shell wiring capture, render, and recording"
```

---

## Task 11: Build, Run, and Fix

**Files:**
- Any files needing fixes from compilation or runtime testing

- [ ] **Step 1: Full build**

Run: `cargo build 2>&1`
Fix any compilation errors. Common issues:
- Import paths needing adjustment
- Lifetime issues with V4L2 stream (may need to restructure)
- Missing `use` statements

- [ ] **Step 2: Run the application**

Run: `RUST_LOG=info cargo run 2>&1`
Expected: Window opens showing the Shadowcast 2 video feed with audio playing through default output. The toolbar toggle button should be visible at the bottom.

- [ ] **Step 3: Test toolbar functionality**

- Click the "Controls" pill to show toolbar
- Adjust volume slider — audio level should change
- Click resolution dropdown — verify formats are listed
- Change resolution — stream should restart at new resolution
- Press `Ctrl+S` — screenshot should save to `~/Pictures/genki-arcade/`
- Press `Ctrl+R` — recording should start, timer should count up
- Press `Ctrl+R` again — recording should stop, MP4 saved to `~/Videos/genki-arcade/`
- Press `F11` — toggle fullscreen
- Press `Escape` — app should exit

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1`
Expected: All unit tests pass.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: build and runtime fixes for initial release"
```

---

## Task 12: Add .gitignore and Clean Up

**Files:**
- Create: `.gitignore`

- [ ] **Step 1: Create .gitignore**

Create `.gitignore`:
```
/target
.superpowers/
*.mp4
*.png
```

- [ ] **Step 2: Commit**

```bash
git add .gitignore
git commit -m "chore: add .gitignore"
```

---

## Verification

After all tasks are complete, verify end-to-end:

1. `cargo test` — all unit tests pass
2. `cargo build --release` — release build succeeds
3. `RUST_LOG=info cargo run --release` — app launches, video displays with low latency
4. Toolbar shows/hides with the pill button
5. Volume slider adjusts audio
6. Resolution dropdown lists all Shadowcast 2 formats and switching works
7. `Ctrl+S` saves PNG to `~/Pictures/genki-arcade/`
8. `Ctrl+R` starts/stops MP4 recording to `~/Videos/genki-arcade/`
9. `F11` toggles fullscreen
10. `Escape` exits cleanly
