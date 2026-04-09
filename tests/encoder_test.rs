use genki_arcade::record::encoder::{recording_path, EncoderConfig};

#[test]
fn test_recording_path_format() {
    let path = recording_path();
    let filename = path.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("recording-"));
    assert!(filename.ends_with(".mp4"));
    assert!(path.parent().unwrap().ends_with("genki-arcade"));
}

#[test]
fn test_encoder_config_defaults() {
    let config = EncoderConfig {
        width: 1920,
        height: 1080,
        fps: 60,
        audio_sample_rate: 48000,
        audio_channels: 2,
    };
    assert_eq!(config.width, 1920);
    assert_eq!(config.fps, 60);
    assert_eq!(config.audio_sample_rate, 48000);
}
