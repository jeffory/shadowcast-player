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
        // Known heights with their standard widths
        let standard = match (self.height, self.width) {
            (480, 640 | 720) => true,
            (576, 720) => true,
            (600, 800) => true,
            (720, 1280) => true,
            (768, 1024) => true,
            (960, 1280) => true,
            (1024, 1280) => true,
            (1080, 1920) => true,
            (1440, 2560) => true,
            _ => false,
        };

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
