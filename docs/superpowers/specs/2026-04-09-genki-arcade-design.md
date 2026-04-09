# genki-arcade — Design Spec

## Overview

A low-latency video capture viewer for the GENKI Shadowcast 2 capture card on Linux. Displays the live video/audio stream with a hidable toolbar for resolution switching, volume control, recording, and screenshots.

**Device**: GENKI ShadowCast 2 (USB 3.2, UVC compliant)
- Video: `/dev/video2` — YUYV and MJPEG, up to 2560x1440@30fps / 1920x1080@60fps
- Audio: ALSA card 3 — 48kHz, 16-bit stereo PCM

## Architecture

Single-binary Rust application with three subsystems connected by channels:

```
V4L2 ─► Frame Decode ─► wgpu Texture ─► Display
  │                         │
  │                    egui Overlay (toolbar)
  │                         │
  └──── (when recording) ──►Encoder Thread ─► MP4 file

cpal Input ─► Ring Buffer ─┬► cpal Output (live playback)
                           └► Encoder Thread (when recording)
```

## Modules

### `capture::video`

V4L2 device management and frame streaming.

- Open `/dev/video2`, enumerate supported formats at startup
- Prefer MJPEG at resolutions >= 1080p (lower USB bandwidth: ~30 MB/s vs ~248 MB/s for YUYV at 1080p60)
- Use YUYV at lower resolutions where bandwidth is not a concern
- Stream frames via mmap buffers (V4L2 memory-mapped I/O)
- Expose a `VideoSource` trait for testability:
  ```rust
  pub trait VideoSource {
      fn supported_formats(&self) -> Vec<CaptureFormat>;
      fn set_format(&mut self, format: &CaptureFormat) -> Result<()>;
      fn next_frame(&mut self) -> Result<Frame>;
  }
  ```

### `capture::audio`

Audio capture and live playback via cpal.

- Open the Shadowcast 2 ALSA capture device (48kHz, 16-bit stereo)
- Route samples through a ring buffer (~20ms / ~1920 samples) to:
  - Default output device for live playback
  - Encoder thread when recording is active
- Volume scaling applied before playback (multiply samples by 0.0..1.0)
- Expose an `AudioSource` trait for testability:
  ```rust
  pub trait AudioSource {
      fn start(&mut self, volume: f32) -> Result<()>;
      fn set_volume(&mut self, volume: f32);
      fn stop(&mut self);
  }
  ```

### `render::display`

Window management and GPU-accelerated video rendering.

- winit window, sized to match the current capture resolution
- wgpu surface with a simple textured-quad pipeline
- Each frame: upload decoded RGB data to a GPU texture, draw fullscreen quad
- Target: present every frame from the capture device (60fps at 1080p60)
- F11 toggles fullscreen (winit `set_fullscreen`)

### `render::overlay`

egui-based toolbar overlay drawn on top of the video.

- **Position**: Centered at the bottom of the window
- **Background**: `rgba(0, 0, 0, 0.75)` with backdrop blur
- **Visibility**: Hidden by default. A small translucent pill button ("Controls") at bottom center toggles it. Auto-hides after 3 seconds of no mouse movement over the toolbar area.
- **Controls** (left to right, centered):
  1. Volume slider (0-100%) with speaker icon
  2. Resolution dropdown — populated from `VideoSource::supported_formats()`, displays as "1080p60", "1440p30", etc.
  3. Record toggle button — red circle icon, changes to stop square when recording
  4. Screenshot button — camera icon
  5. Recording timer — "00:00:00" format, visible only when recording

### `record::encoder`

H.264 + AAC encoding to MP4 via ffmpeg bindings.

- Runs on a dedicated thread, receives frames/audio via bounded channels
- On record start:
  - Create MP4 file at `~/Videos/genki-arcade/recording-{YYYY-MM-DD_HH-MM-SS}.mp4`
  - Initialize H.264 encoder (match capture resolution and framerate)
  - Initialize AAC encoder (48kHz stereo)
- On record stop: flush encoders, finalize MP4 container, join thread
- Channel backpressure: if encoder falls behind, drop oldest frames (never block the display pipeline)
- Expose an `Encoder` trait for testability:
  ```rust
  pub trait Encoder {
      fn start(&mut self, config: EncoderConfig) -> Result<()>;
      fn send_video_frame(&mut self, frame: &Frame) -> Result<()>;
      fn send_audio_samples(&mut self, samples: &[i16]) -> Result<()>;
      fn stop(&mut self) -> Result<PathBuf>;
  }
  ```

### `record::screenshot`

PNG screenshot capture.

- Grab the current decoded frame (not from GPU — use the pre-upload RGB buffer)
- Save as PNG to `~/Pictures/genki-arcade/screenshot-{YYYY-MM-DD_HH-MM-SS}.png`
- Non-blocking: encode PNG on a spawned thread

## Data Types

```rust
/// A supported capture format from V4L2
pub struct CaptureFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pixel_format: PixelFormat, // MJPEG or YUYV
}

/// A decoded video frame ready for display
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,        // RGB24
    pub timestamp: Instant,
}

/// Encoder configuration derived from current capture format
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio_sample_rate: u32,  // 48000
    pub audio_channels: u16,     // 2
}

pub enum PixelFormat {
    Mjpeg,
    Yuyv,
}
```

## Frame Decode

Two decode paths based on the active pixel format:

- **MJPEG -> RGB**: Use `zune-jpeg` crate for pure-Rust JPEG decoding. At 1080p60 this is ~16ms budget per frame; zune-jpeg typically decodes in <5ms.
- **YUYV -> RGB**: Direct colorspace conversion. Each 4 bytes of YUYV produces 2 RGB pixels. Simple loop, no external dependency.

Both paths produce `Frame { data: Vec<u8> }` with RGB24 pixel data.

## Resolution Switching

1. User selects a new resolution from the toolbar dropdown
2. Stop the current V4L2 stream (`VIDIOC_STREAMOFF`)
3. Set the new format (`VIDIOC_S_FMT`)
4. Restart the stream (`VIDIOC_STREAMON`)
5. Resize the winit window to match the new resolution
6. Recreate the wgpu texture at the new dimensions

During the switch (~100-500ms), display the last frame or a black screen. No audio interruption needed.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Device disconnected | Show "Signal Lost" overlay, poll `/dev/video2` for reconnection every 2 seconds |
| Encoder failure | Stop recording, show error toast in overlay for 5 seconds, keep preview running |
| Format negotiation fails | Fall back to next supported resolution in descending order |
| Audio device unavailable | Continue video-only, show muted icon in toolbar |
| Screenshot write fails | Show error toast in overlay for 5 seconds |

## Key Bindings

| Key | Action |
|-----|--------|
| `F11` | Toggle fullscreen |
| `Ctrl+S` | Screenshot |
| `Ctrl+R` | Toggle recording |
| `Escape` | Exit application |

## File Output

- **Recordings**: `~/Videos/genki-arcade/recording-{YYYY-MM-DD_HH-MM-SS}.mp4`
- **Screenshots**: `~/Pictures/genki-arcade/screenshot-{YYYY-MM-DD_HH-MM-SS}.png`
- Directories created on first use via `directories` crate (XDG paths)

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `v4l` | latest | V4L2 bindings for video capture |
| `cpal` | latest | Cross-platform audio I/O |
| `wgpu` | latest | GPU-accelerated rendering |
| `winit` | latest | Window management |
| `egui` | latest | Immediate-mode GUI for toolbar |
| `egui-wgpu` | latest | egui wgpu rendering backend |
| `egui-winit` | latest | egui winit integration |
| `zune-jpeg` | latest | MJPEG frame decoding |
| `ffmpeg-next` | latest | H.264/AAC encoding, MP4 muxing |
| `image` | latest | PNG encoding for screenshots |
| `directories` | latest | XDG base directory paths |
| `chrono` | latest | Timestamps for filenames |
| `crossbeam-channel` | latest | Bounded MPSC channels for frame/audio transport |
| `anyhow` | latest | Error handling |
| `ringbuf` | latest | Lock-free ring buffer for audio |

## Testing Strategy

### Trait-Based Abstraction

`VideoSource`, `AudioSource`, and `Encoder` traits enable unit testing without hardware. Mock implementations return synthetic frames, silence, and no-op encoding.

### Unit Tests

- **Frame conversion**: YUYV->RGB correctness (known input/output pairs), MJPEG decode of valid/corrupt JPEG data
- **Audio volume scaling**: Verify sample multiplication at 0%, 50%, 100% volumes, clipping behavior
- **Resolution parsing**: `CaptureFormat` display formatting ("1080p60", "1440p30")
- **Timestamp formatting**: Recording timer display, filename timestamp generation
- **Channel backpressure**: Verify frames are dropped (not blocking) when encoder is slow

### Integration Tests

- V4L2 device enumeration returns expected formats (requires Shadowcast 2 connected)
- cpal lists the Shadowcast 2 audio device
- Full capture->encode->file pipeline produces a valid MP4 (requires device)

### What We Don't Test

- wgpu rendering output (GPU-dependent, verified visually)
- egui layout (immediate-mode, verified visually)
- End-to-end latency (measured with instrumentation, not automated tests)
