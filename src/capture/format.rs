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

/// Convert YUYV (4:2:2) buffer to RGB24.
/// Uses BT.601 studio-range conversion.
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

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub timestamp: Instant,
}
