use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

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
        let standard = matches!(
            (self.height, self.width),
            (480, 640 | 720)
                | (576, 720)
                | (600, 800)
                | (720, 1280)
                | (768, 1024)
                | (960, 1280)
                | (1024, 1280)
                | (1080, 1920)
                | (1440, 2560)
        );

        if standard {
            write!(f, "{}p{}", self.height, self.fps)
        } else {
            write!(f, "{}x{}@{}", self.width, self.height, self.fps)
        }
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Pixel buffer shared via `Arc` so the host can fan out frames to the
    /// renderer / recorder / plugins with a refcount bump instead of cloning
    /// the whole buffer (≈8 MB at 1080p BGRA) on every frame.
    pub data: Arc<Vec<u8>>,
    pub timestamp: Instant,
}

/// Modifier-key bitmask layout matching the `pico-keeb-protocol`. The host
/// produces this directly on the `KeyboardInput` hot path so plugins (the
/// pico-keeb plugin in particular) don't need their own per-platform
/// translation table.
pub mod modifier {
    pub const MOD_LCTRL: u8 = 0x01;
    pub const MOD_LSHIFT: u8 = 0x02;
    pub const MOD_LALT: u8 = 0x04;
    pub const MOD_LGUI: u8 = 0x08;
    pub const MOD_RCTRL: u8 = 0x10;
    pub const MOD_RSHIFT: u8 = 0x20;
    pub const MOD_RALT: u8 = 0x40;
    pub const MOD_RGUI: u8 = 0x80;
}

/// Mouse button identifiers forwarded over [`AppEvent::MouseButton`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    DeviceConnected {
        name: String,
    },
    DeviceDisconnected,
    RecordingStarted {
        path: PathBuf,
    },
    RecordingStopped {
        path: PathBuf,
    },
    FormatChanged {
        format: CaptureFormat,
    },
    WindowResized {
        width: u32,
        height: u32,
    },
    AudioStateChanged {
        active: bool,
    },

    /// A keyboard key press / release, forwarded by the host only while
    /// capture mode is active. `key_code` is the USB HID Usage ID (Keyboard /
    /// Keypad page, see pico-keeb-protocol); `scan_code` is the platform-
    /// native scancode for plugins that need raw HW codes. `modifiers` is the
    /// pico-keeb-protocol bitmask (see the [`modifier`] module). `repeat` is
    /// true when the OS auto-repeated the key, false on the initial press
    /// and on release.
    KeyboardInput {
        key_code: u32,
        scan_code: u32,
        modifiers: u8,
        pressed: bool,
        repeat: bool,
    },

    /// Relative mouse motion in pixels since the last event. Sourced from
    /// `DeviceEvent::MouseMotion`, so it is raw and unaffected by cursor
    /// clamping at the screen edge.
    MouseMotion {
        dx: f32,
        dy: f32,
    },

    /// A mouse button press / release while capture mode is active.
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },

    /// Mouse-wheel / scroll delta in line-units (winit's `LineDelta`; pixel
    /// deltas are approximated to lines by the host).
    MouseScroll {
        dx: f32,
        dy: f32,
    },

    /// Emitted whenever the host's capture mode toggles. Plugins should use
    /// this to release any keys they considered "held" — the host stops
    /// forwarding releases when capture turns off mid-press.
    CaptureModeChanged {
        active: bool,
    },
}

#[derive(Debug)]
pub enum AppCommand {
    TakeScreenshot,
    StartRecording,
    StopRecording,
    SetFormat(CaptureFormat),
    ToggleFullscreen,
    Quit,
}

pub struct PluginContext {
    pub frame_rx: Receiver<Arc<Frame>>,
    pub event_rx: Receiver<AppEvent>,
    pub command_tx: Sender<AppCommand>,
    pub config: toml::Table,
    pub stop_flag: Arc<std::sync::atomic::AtomicBool>,
}

pub trait Plugin: Send + 'static {
    fn name(&self) -> &str;
    fn run(&mut self, ctx: PluginContext);
    fn stop(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_format_display_standard() {
        let fmt = CaptureFormat {
            width: 1920,
            height: 1080,
            fps: 60,
            pixel_format: PixelFormat::Mjpeg,
        };
        assert_eq!(fmt.to_string(), "1080p60");
    }

    #[test]
    fn capture_format_display_non_standard() {
        let fmt = CaptureFormat {
            width: 1360,
            height: 768,
            fps: 60,
            pixel_format: PixelFormat::Mjpeg,
        };
        assert_eq!(fmt.to_string(), "1360x768@60");
    }

    #[test]
    fn app_event_is_clone() {
        let event = AppEvent::DeviceConnected {
            name: "test".into(),
        };
        let _cloned = event.clone();
    }

    #[test]
    fn plugin_context_channels_work() {
        let (frame_tx, frame_rx) = crossbeam_channel::bounded(4);
        let (event_tx, event_rx) = crossbeam_channel::bounded(4);
        let (command_tx, command_rx) = crossbeam_channel::bounded(4);

        let ctx = PluginContext {
            frame_rx,
            event_rx,
            command_tx,
            config: toml::Table::new(),
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        let frame = Arc::new(Frame {
            width: 1920,
            height: 1080,
            data: Arc::new(vec![0u8; 1920 * 1080 * 3]),
            timestamp: Instant::now(),
        });
        frame_tx.send(frame).unwrap();
        let received = ctx.frame_rx.recv().unwrap();
        assert_eq!(received.width, 1920);

        event_tx
            .send(AppEvent::DeviceConnected {
                name: "test".into(),
            })
            .unwrap();
        let event = ctx.event_rx.recv().unwrap();
        assert!(matches!(event, AppEvent::DeviceConnected { .. }));

        ctx.command_tx.send(AppCommand::TakeScreenshot).unwrap();
        let cmd = command_rx.recv().unwrap();
        assert!(matches!(cmd, AppCommand::TakeScreenshot));
    }
}
