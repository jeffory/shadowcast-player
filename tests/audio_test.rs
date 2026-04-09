use genki_arcade::capture::audio::scale_volume;

#[test]
fn test_scale_volume_full() {
    let samples = vec![1000i16, -1000, 500, -500];
    let scaled = scale_volume(&samples, 1.0);
    assert_eq!(scaled, vec![1000, -1000, 500, -500]);
}

#[test]
fn test_scale_volume_half() {
    let samples = vec![1000i16, -1000, 500, -500];
    let scaled = scale_volume(&samples, 0.5);
    assert_eq!(scaled, vec![500, -500, 250, -250]);
}

#[test]
fn test_scale_volume_mute() {
    let samples = vec![1000i16, -1000, 32767, -32768];
    let scaled = scale_volume(&samples, 0.0);
    assert_eq!(scaled, vec![0, 0, 0, 0]);
}

#[test]
fn test_scale_volume_clipping() {
    let samples = vec![32767i16, -32768];
    let scaled = scale_volume(&samples, 1.0);
    assert_eq!(scaled[0], 32767);
    assert_eq!(scaled[1], -32768);
}
