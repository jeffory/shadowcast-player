use genki_arcade::capture::format::{yuyv_to_rgb, CaptureFormat, PixelFormat};

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

#[test]
fn test_yuyv_to_rgb_single_macropixel() {
    // YUYV macropixel: Y0=128, U=128, Y1=128, V=128 -> neutral gray
    let yuyv = vec![128, 128, 128, 128];
    let rgb = yuyv_to_rgb(&yuyv, 2, 1);
    assert_eq!(rgb.len(), 6); // 2 pixels * 3 bytes
    // Neutral gray: Y=128, U=128, V=128 -> R≈128, G≈128, B≈128
    assert!((rgb[0] as i32 - 128).abs() < 3);
    assert!((rgb[1] as i32 - 128).abs() < 3);
    assert!((rgb[2] as i32 - 128).abs() < 3);
    assert!((rgb[3] as i32 - 128).abs() < 3);
    assert!((rgb[4] as i32 - 128).abs() < 3);
    assert!((rgb[5] as i32 - 128).abs() < 3);
}

#[test]
fn test_yuyv_to_rgb_black() {
    let yuyv = vec![16, 128, 16, 128];
    let rgb = yuyv_to_rgb(&yuyv, 2, 1);
    assert!(rgb[0] < 10);
    assert!(rgb[1] < 10);
    assert!(rgb[2] < 10);
}

#[test]
fn test_yuyv_to_rgb_white() {
    let yuyv = vec![235, 128, 235, 128];
    let rgb = yuyv_to_rgb(&yuyv, 2, 1);
    assert!(rgb[0] > 245);
    assert!(rgb[1] > 245);
    assert!(rgb[2] > 245);
}

#[test]
fn test_yuyv_to_rgb_output_size() {
    let yuyv = vec![128u8; 16];
    let rgb = yuyv_to_rgb(&yuyv, 4, 2);
    assert_eq!(rgb.len(), 24);
}
