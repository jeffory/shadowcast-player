use anyhow::{Context, Result};
use chrono::Local;
use directories::UserDirs;
use image::{ImageBuffer, Rgb};
use std::path::PathBuf;

use crate::capture::format::FramePixelFormat;

/// Generate the output path for a screenshot.
pub fn screenshot_path() -> PathBuf {
    let base = UserDirs::new()
        .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = base.join("shadowcast-player");
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    dir.join(format!("screenshot-{}.png", timestamp))
}

/// Save a captured frame as a PNG screenshot. Runs on a spawned thread.
pub fn take_screenshot(data: Vec<u8>, width: u32, height: u32, format: FramePixelFormat) {
    std::thread::spawn(move || {
        if let Err(e) = save_png(&data, width, height, format) {
            log::error!("Screenshot failed: {}", e);
        }
    });
}

fn save_png(data: &[u8], width: u32, height: u32, format: FramePixelFormat) -> Result<()> {
    let path = screenshot_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {:?}", parent))?;
    }

    let rgb_data: Vec<u8> = match format {
        FramePixelFormat::Rgb8 => data.to_vec(),
        FramePixelFormat::Bgra8 => {
            let mut out = Vec::with_capacity((width * height * 3) as usize);
            for px in data.chunks_exact(4) {
                out.push(px[2]); // R
                out.push(px[1]); // G
                out.push(px[0]); // B
            }
            out
        }
    };

    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(width, height, rgb_data)
        .context("Failed to create image buffer from pixel data")?;

    img.save(&path)
        .with_context(|| format!("Failed to save screenshot to {:?}", path))?;

    log::info!("Screenshot saved to {:?}", path);
    Ok(())
}
