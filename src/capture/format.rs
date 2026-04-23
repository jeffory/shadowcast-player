use std::fmt;
use std::sync::Arc;
use std::time::Instant;
use zune_jpeg::JpegDecoder;

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

/// Layout of pixel data in `Frame.data`.
///
/// The native macOS path keeps BGRA end-to-end so the GPU can do the channel
/// swizzle via the texture format (wgpu's `Bgra8UnormSrgb`). Linux and Windows
/// still decode MJPEG/YUYV to RGB24 on the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePixelFormat {
    /// 4 bytes per pixel, order B, G, R, A. Produced by AVFoundation.
    Bgra8,
    /// 3 bytes per pixel, order R, G, B. Produced by the v4l2 and Media
    /// Foundation backends after MJPEG/YUYV decode.
    Rgb8,
}

impl FramePixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            FramePixelFormat::Bgra8 => 4,
            FramePixelFormat::Rgb8 => 3,
        }
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Pixel buffer shared via `Arc` so fan-out to the recorder / screenshot /
    /// last-frame slot is a cheap refcount bump instead of a full `Vec` clone
    /// (at 1080p60 BGRA that is ~475 MB/s of allocator traffic avoided).
    pub data: Arc<Vec<u8>>,
    pub pixel_format: FramePixelFormat,
    pub timestamp: Instant,
}

/// Decode an MJPEG frame (JPEG buffer) to RGB24.
/// Returns (rgb_data, width, height).
pub fn mjpeg_to_rgb(jpeg_data: &[u8]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let cursor = std::io::Cursor::new(jpeg_data);
    let mut decoder = JpegDecoder::new(cursor);
    decoder
        .decode_headers()
        .map_err(|e| anyhow::anyhow!("JPEG header error: {:?}", e))?;

    let info = decoder
        .info()
        .ok_or_else(|| anyhow::anyhow!("No dimensions in JPEG"))?;
    let width = info.width as u32;
    let height = info.height as u32;

    let pixels = decoder
        .decode()
        .map_err(|e| anyhow::anyhow!("JPEG decode error: {:?}", e))?;

    Ok((pixels, width, height))
}
