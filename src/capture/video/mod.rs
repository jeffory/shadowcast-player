use anyhow::Result;

use super::format::{CaptureFormat, Frame};

/// Trait for video capture sources.
pub trait VideoSource {
    /// Returns all supported capture formats, sorted by resolution (highest first)
    /// then fps (highest first).
    fn supported_formats(&self) -> Vec<CaptureFormat>;

    /// Configures the device to use the given format.
    fn set_format(&mut self, format: &CaptureFormat) -> Result<()>;

    /// Starts the capture stream.
    fn start(&mut self) -> Result<()>;

    /// Returns the next decoded RGB frame from the stream.
    fn next_frame(&mut self) -> Result<Frame>;

    /// Stops the capture stream.
    fn stop(&mut self) -> Result<()>;
}

#[cfg(target_os = "linux")]
mod v4l2;
#[cfg(target_os = "linux")]
pub use v4l2::V4l2Source as PlatformVideoSource;

#[cfg(target_os = "macos")]
mod avfoundation;
#[cfg(target_os = "macos")]
pub use avfoundation::AvFoundationSource as PlatformVideoSource;

#[cfg(target_os = "windows")]
mod mediafoundation;
#[cfg(target_os = "windows")]
pub use mediafoundation::MediaFoundationSource as PlatformVideoSource;
