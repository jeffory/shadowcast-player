use genki_arcade::capture::format::{CaptureFormat, PixelFormat};

#[test]
fn test_capture_format_display_1080p60() {
    let fmt = CaptureFormat { width: 1920, height: 1080, fps: 60, pixel_format: PixelFormat::Mjpeg };
    assert_eq!(fmt.to_string(), "1080p60");
}

#[test]
fn test_capture_format_display_1440p30() {
    let fmt = CaptureFormat { width: 2560, height: 1440, fps: 30, pixel_format: PixelFormat::Yuyv };
    assert_eq!(fmt.to_string(), "1440p30");
}

#[test]
fn test_capture_format_display_720p60() {
    let fmt = CaptureFormat { width: 1280, height: 720, fps: 60, pixel_format: PixelFormat::Mjpeg };
    assert_eq!(fmt.to_string(), "720p60");
}

#[test]
fn test_capture_format_display_non_standard() {
    let fmt = CaptureFormat { width: 1360, height: 768, fps: 60, pixel_format: PixelFormat::Mjpeg };
    assert_eq!(fmt.to_string(), "1360x768@60");
}
