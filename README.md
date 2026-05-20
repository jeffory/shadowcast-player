# shadowcast-player

A cross-platform media player for the [Genki ShadowCast 2](https://www.genkithings.com/products/shadowcast) USB capture device, built in Rust. Captures live video and audio from the device and renders it in a GPU-accelerated window with recording and screenshot support.

## Features

- **Live video capture** - Real-time MJPEG/YUYV video from the ShadowCast 2
- **Audio passthrough** - Captures USB audio and plays it through your default output device
- **Recording** - Record to H.264 + AAC MP4 files; uses a hardware encoder (VideoToolbox on macOS, Media Foundation / NVENC / QSV / AMF on Windows) when available, falling back to libx264
- **Screenshots** - Save the current frame as PNG
- **Format selection** - Choose resolution, frame rate, and pixel format from device capabilities
- **Scaling modes** - Fit, Fill, Stretch, and 100% (native resolution)
- **Volume control** - Adjustable via the toolbar overlay
- **Auto-reconnect** - Automatically reconnects when the device is unplugged and reattached. Audio retry uses exponential backoff (2s → 4s → 8s → 30s) so a missing audio device doesn't stall the render thread
- **Frame-stats overlay** (F12) - Live counters for captured / rendered / dropped frames and peak frame time, with once-per-second log lines
- **Zero-copy capture path on macOS** - BGRA pixel data from AVFoundation flows straight to the GPU via a `Bgra8UnormSrgb` texture; no CPU colorspace conversion on the hot path
- **Cross-platform** - Linux (V4L2), macOS (AVFoundation), Windows (MediaFoundation)

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Esc` / `Q` | Quit |
| `F11` | Toggle fullscreen |
| `F12` | Toggle frame-stats overlay |
| `Ctrl+S` | Take screenshot |
| `Ctrl+R` | Toggle recording |
| `Right Ctrl` | Toggle input-capture mode (forwards keyboard / mouse to plugins; configurable via `[capture] toggle`) |

The toolbar overlay appears when you move the mouse and auto-hides after a short delay (VLC-style).

While input-capture mode is on, the player's own shortcuts (`Esc`, `F11`, `F12`, `Ctrl+S`, `Ctrl+R`) are suppressed — only the configured toggle key still acts locally — so every other keystroke can be forwarded verbatim to plugins like `pico-keeb`. A small `INPUT CAPTURED — press <toggle> to release` pill in the top-right corner makes the state obvious.

### Frame-stats overlay

Press `F12` to show a small overlay in the top-left corner with live pipeline counters, refreshed once per second:

```
captured 60/s   rendered 60/s
dropped at capture 0/s
peak frame 14.22 ms
```

- **captured** — frames delivered by the capture backend.
- **rendered** — frames that reached the GPU and were presented.
- **dropped at capture** — frames the backend discarded because the render thread's channel was full. Non-zero means the render loop can't keep up; the label turns red.
- **peak frame** — worst-case `RedrawRequested` handler duration in the last second. Amber above 18 ms, red above 25 ms (at 60 fps the vsync budget is 16.67 ms).

Enabling the overlay also logs the same line to the console (`RUST_LOG=info`) so you have a record across a session.

## Plugins

The player ships with a small in-process plugin system. Each plugin runs on its own thread, receives a copy of every captured frame plus app events (device connect / disconnect, format change, recording start / stop, keyboard + mouse while in capture mode), and can drive the host back via a small command channel (take a screenshot, start / stop recording, set format, toggle fullscreen, quit).

### Enabling a plugin

Plugins are gated by Cargo features so a release build only links what you actually use:

```bash
cargo build --release --features "example-logger pico-keeb"
```

Then opt each one in from your `shadowcast.toml`:

```toml
[plugins.example-logger]
enabled = true
```

Config keys other than `enabled` are passed through to the plugin verbatim. The example config at `shadowcast.example.toml` shows the full per-plugin shape; copy it to your platform's config directory (`~/.config/shadowcast-player/` on Linux, `~/Library/Application Support/shadowcast-player/` on macOS, `%APPDATA%\shadowcast-player\` on Windows).

### Available plugins

- **`example-logger`** *(built by default)* — Logs the high-level event stream (`DeviceConnected`, `FormatChanged`, `RecordingStarted`, …) to `RUST_LOG=info`. Handy as a sanity check that the plugin host is wired up correctly, and as a template for new plugins.

- **`pico-keeb`** — Forwards captured keyboard and mouse input as USB HID frames to a [pico-keeb](https://github.com/jeffory/pico-keeb) RP2040 board over a serial port. The board replays them as a real HID device to a downstream target PC, so you can use the same keyboard + mouse for your host and the captured machine without a hardware KVM. Build with `--features pico-keeb` and configure with:

  ```toml
  [plugins.pico-keeb]
  enabled = true
  port = "/dev/ttyUSB0"      # required: serial device path
  baud = 921600              # default 921600
  forward_mouse = true       # set false to forward keyboard only
  key_hold_ms = 0            # raise to ~30 for MiSTer-style chatter-filter targets
  ```

  Capture mode is off by default. Press the toggle key (`Right Ctrl` out of the box) to start forwarding, and press it again to release. While capture is active the player's own keyboard shortcuts are suppressed (only the toggle still acts locally), the cursor is locked into the window so mouse motion is delivered as relative deltas, and an `INPUT CAPTURED` indicator pill is drawn in the top-right corner. See the [pico-keeb hardware repo](https://github.com/jeffory/pico-keeb) for the board side.

  To pick a different toggle, add a `[capture]` block to your config:

  ```toml
  [capture]
  toggle = "ScrollLock"   # any modifier alias (RightCtrl, LeftAlt, …), F1..F12, ScrollLock, Pause, CapsLock
  mouse = true            # set false to forward keyboard only, even on enabled plugins
  ```

## Building

### Prerequisites

**Linux:**
```bash
sudo apt-get install libv4l-dev libasound2-dev libavcodec-dev libavformat-dev \
  libavutil-dev libswscale-dev libswresample-dev pkg-config clang
```

**macOS:**
```bash
brew install ffmpeg pkg-config
```

**Windows:**
```bash
vcpkg install ffmpeg:x64-windows
```

### Build & Run

```bash
cargo build --release
./target/release/shadowcast-player
```

### Tests

```bash
cargo test
```

## Architecture

```
src/
├── main.rs            Entry point
├── app.rs             Application state machine and event loop
├── stats.rs           FrameStats counters and StatsTicker (F12 overlay + logging)
├── capture/
│   ├── device.rs      ShadowCast 2 USB device discovery
│   ├── format.rs      Capture formats, Frame struct, MJPEG/YUYV → RGB decoders
│   ├── audio.rs       Audio capture via cpal (format-adaptive i16/f32)
│   └── video/
│       ├── mod.rs     VideoSource trait
│       ├── v4l2.rs    Linux V4L2 backend (RGB8 output)
│       ├── avfoundation.rs   macOS backend (BGRA8 pass-through)
│       └── mediafoundation.rs   Windows backend (RGB8 output)
├── render/
│   ├── display.rs     wgpu GPU renderer with per-format texture upload
│   └── overlay.rs     egui toolbar UI
└── record/
    ├── encoder.rs     FFmpeg H.264+AAC MP4 encoder (hardware-preferred, wall-clock PTS)
    └── screenshot.rs  PNG screenshot capture
```

### Key Libraries

| Library | Purpose |
|---------|---------|
| [wgpu](https://crates.io/crates/wgpu) | GPU rendering (Vulkan/Metal/DX12) |
| [winit](https://crates.io/crates/winit) | Cross-platform windowing |
| [egui](https://crates.io/crates/egui) | Immediate-mode GUI for toolbar |
| [cpal](https://crates.io/crates/cpal) | Cross-platform audio I/O |
| [ffmpeg-next](https://crates.io/crates/ffmpeg-next) | Video/audio encoding |
| [zune-jpeg](https://crates.io/crates/zune-jpeg) | MJPEG decoding |

## Recording Output

Recordings are saved to your platform's video directory:

- **Linux:** `~/.local/share/videos/shadowcast-player/`
- **macOS:** `~/Movies/shadowcast-player/`
- **Windows:** `Videos\shadowcast-player\`

Files are named `recording-YYYY-MM-DD_HH-MM-SS.mp4`.

### Encoder selection

At recording start the app tries to find a platform hardware H.264 encoder and falls back to `libx264` if none are available. Preference order per platform:

| Platform | Preference |
|----------|------------|
| macOS | `h264_videotoolbox` → `libx264` |
| Windows | `h264_mf` → `h264_nvenc` → `h264_qsv` → `h264_amf` → `libx264` |
| Linux | `libx264` (no hardware path wired up yet) |

The chosen encoder is logged at the `info` level (`Using hardware H.264 encoder: h264_videotoolbox`). B-frames are disabled on hardware paths where they tend to cause reordering artifacts.

Video PTS is derived from wall-clock elapsed time (not a monotonic frame counter), so if the encoder or an intermediate channel ever has to drop frames the recording's duration still matches real time — the output simply runs at a lower effective frame rate rather than compressing content into a shorter clip.
