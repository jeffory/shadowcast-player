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
    pub data: Vec<u8>,
    pub timestamp: Instant,
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    DeviceConnected { name: String },
    DeviceDisconnected,
    RecordingStarted { path: PathBuf },
    RecordingStopped { path: PathBuf },
    FormatChanged { format: CaptureFormat },
    WindowResized { width: u32, height: u32 },
    AudioStateChanged { active: bool },
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
        };

        let frame = Arc::new(Frame {
            width: 1920,
            height: 1080,
            data: vec![0u8; 1920 * 1080 * 3],
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
