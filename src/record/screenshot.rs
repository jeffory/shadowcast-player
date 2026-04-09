use anyhow::{Context, Result};
use chrono::Local;
use directories::UserDirs;
use image::{ImageBuffer, Rgb};
use std::path::PathBuf;

/// Generate the output path for a screenshot.
pub fn screenshot_path() -> PathBuf {
    let base = UserDirs::new()
        .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = base.join("genki-arcade");
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    dir.join(format!("screenshot-{}.png", timestamp))
}

/// Save an RGB24 buffer as a PNG screenshot. Runs on a spawned thread.
pub fn take_screenshot(rgb_data: Vec<u8>, width: u32, height: u32) {
    std::thread::spawn(move || {
        if let Err(e) = save_png(&rgb_data, width, height) {
            log::error!("Screenshot failed: {}", e);
        }
    });
}

fn save_png(rgb_data: &[u8], width: u32, height: u32) -> Result<()> {
    let path = screenshot_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {:?}", parent))?;
    }

    let img: ImageBuffer<Rgb<u8>, _> =
        ImageBuffer::from_raw(width, height, rgb_data.to_vec())
            .context("Failed to create image buffer from RGB data")?;

    img.save(&path)
        .with_context(|| format!("Failed to save screenshot to {:?}", path))?;

    log::info!("Screenshot saved to {:?}", path);
    Ok(())
}
