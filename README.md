# shadowcast-player

A cross-platform media player for the [Genki ShadowCast 2](https://www.genkithings.com/products/shadowcast) USB capture device, built in Rust. Captures live video and audio from the device and renders it in a GPU-accelerated window with recording and screenshot support.

## Features

- **Live video capture** - Real-time MJPEG/YUYV video from the ShadowCast 2
- **Audio passthrough** - Captures USB audio and plays it through your default output device
- **Recording** - Record to H.264 + AAC MP4 files
- **Screenshots** - Save the current frame as PNG
- **Format selection** - Choose resolution, frame rate, and pixel format from device capabilities
- **Scaling modes** - Fit, Fill, Stretch, and 100% (native resolution)
- **Volume control** - Adjustable via the toolbar overlay
- **Auto-reconnect** - Automatically reconnects when the device is unplugged and reattached
- **Cross-platform** - Linux (V4L2), macOS (AVFoundation), Windows (MediaFoundation)

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Esc` / `Q` | Quit |
| `F11` | Toggle fullscreen |
| `Ctrl+S` | Take screenshot |
| `Ctrl+R` | Toggle recording |

The toolbar overlay appears when you move the mouse and auto-hides after a short delay (VLC-style).

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
├── capture/
│   ├── device.rs      ShadowCast 2 USB device discovery
│   ├── format.rs      Capture formats and codec conversion (MJPEG/YUYV → RGB)
│   ├── audio.rs       Audio capture via cpal
│   └── video/
│       ├── mod.rs     VideoSource trait
│       ├── v4l2.rs    Linux V4L2 backend
│       ├── avfoundation.rs   macOS backend
│       └── mediafoundation.rs   Windows backend
├── render/
│   ├── display.rs     wgpu GPU renderer with scaling modes
│   └── overlay.rs     egui toolbar UI
└── record/
    ├── encoder.rs     FFmpeg H.264+AAC MP4 encoder
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
