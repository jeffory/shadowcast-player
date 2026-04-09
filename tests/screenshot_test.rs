use genki_arcade::record::screenshot::screenshot_path;

#[test]
fn test_screenshot_path_format() {
    let path = screenshot_path();
    let filename = path.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("screenshot-"));
    assert!(filename.ends_with(".png"));
    assert!(path.parent().unwrap().ends_with("genki-arcade"));
}

#[test]
fn test_screenshot_paths_are_unique() {
    let path1 = screenshot_path();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let path2 = screenshot_path();
    assert_ne!(path1, path2);
}
