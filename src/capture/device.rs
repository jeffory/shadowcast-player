/// Known USB vendor/product IDs for the Genki ShadowCast 2.
#[cfg(target_os = "linux")]
const SHADOWCAST_VENDOR_ID: &str = "345f";
#[cfg(target_os = "linux")]
const SHADOWCAST_PRODUCT_ID: &str = "2013";

/// Represents a discovered video capture device.
#[derive(Debug, Clone)]
pub struct CaptureDevice {
    pub name: String,
    /// Platform-specific device path or identifier.
    /// On Linux this is e.g. "/dev/video2", on other platforms a device ID string.
    pub video_path: String,
    /// Substrings to match when finding the corresponding audio input device in cpal.
    /// Tried in order; the first match wins. Multiple entries handle platform differences
    /// (e.g. ALSA uses card names like "CARD=S2" while macOS/Windows use product names).
    pub audio_matches: Vec<String>,
}

/// Attempts to find a connected Genki ShadowCast device.
///
/// Returns the first matching device, or None if no ShadowCast is connected.
pub fn find_shadowcast() -> Option<CaptureDevice> {
    #[cfg(target_os = "linux")]
    return find_shadowcast_linux();

    #[cfg(target_os = "macos")]
    return find_shadowcast_macos();

    #[cfg(target_os = "windows")]
    return find_shadowcast_windows();
}

/// Enumerates all available video capture devices.
pub fn enumerate_devices() -> Vec<CaptureDevice> {
    #[cfg(target_os = "linux")]
    return enumerate_devices_linux();

    #[cfg(target_os = "macos")]
    return enumerate_devices_macos();

    #[cfg(target_os = "windows")]
    return enumerate_devices_windows();
}

// --- Linux implementation ---

#[cfg(target_os = "linux")]
fn find_shadowcast_linux() -> Option<CaptureDevice> {
    let devices = enumerate_devices_linux();

    // First try USB vendor/product ID match (most reliable)
    for device in &devices {
        let sysfs_path = std::path::Path::new("/sys/class/video4linux")
            .join(device.video_path.trim_start_matches("/dev/"));
        if is_shadowcast_usb_device(&sysfs_path) {
            return Some(device.clone());
        }
    }

    // Fall back to name matching
    devices
        .into_iter()
        .find(|d| d.name.to_lowercase().contains("shadowcast"))
}

#[cfg(target_os = "linux")]
fn enumerate_devices_linux() -> Vec<CaptureDevice> {
    let mut devices = Vec::new();

    let video_dir = match std::fs::read_dir("/sys/class/video4linux") {
        Ok(dir) => dir,
        Err(_) => return devices,
    };

    for entry in video_dir.flatten() {
        let sysfs_path = entry.path();
        let dev_name = entry.file_name().to_string_lossy().to_string();

        // Read the device name from sysfs
        let name_path = sysfs_path.join("name");
        let name = std::fs::read_to_string(&name_path)
            .unwrap_or_default()
            .trim()
            .to_string();

        if name.is_empty() {
            continue;
        }

        let device_path = format!("/dev/{}", dev_name);

        // Build a list of audio match candidates.
        // ALSA device names use patterns like "sysdefault:CARD=S2" or
        // "front:CARD=S2,DEV=0", so we need the ALSA card name from the
        // sibling sound device in sysfs, not the human-readable product name.
        let alsa_card = find_sibling_alsa_card(&sysfs_path);
        let mut audio_matches = Vec::new();

        if let Some(card) = &alsa_card {
            // ALSA card name is the most reliable match on Linux
            audio_matches.push(format!("CARD={}", card));
        }
        // Also try the product name for non-ALSA backends (PipeWire, PulseAudio)
        // which may expose the full device name
        audio_matches.push(name.clone());

        devices.push(CaptureDevice {
            name,
            video_path: device_path,
            audio_matches,
        });
    }

    // Sort by device path for stable ordering
    devices.sort_by(|a, b| a.video_path.cmp(&b.video_path));
    devices
}

/// Finds the ALSA card name for a USB device by looking for a sibling "sound"
/// directory in the parent USB interface's sysfs tree.
///
/// For a video device at e.g. /sys/class/video4linux/video2, we walk up to the
/// parent USB device and look for a sound/card*/id file which contains the ALSA
/// card name (e.g. "S2").
#[cfg(target_os = "linux")]
fn find_sibling_alsa_card(video_sysfs_path: &std::path::Path) -> Option<String> {
    // Resolve the "device" symlink to get the actual sysfs device path
    let device_link = video_sysfs_path.join("device");
    let resolved = std::fs::canonicalize(&device_link).ok()?;

    // Walk up to find the USB device (the directory with idVendor/idProduct)
    let mut usb_device = resolved.as_path();
    for _ in 0..10 {
        if usb_device.join("idVendor").exists() {
            break;
        }
        usb_device = usb_device.parent()?;
    }

    if !usb_device.join("idVendor").exists() {
        return None;
    }

    // Search all subdirectories recursively for sound/card*/id
    // The USB device may have multiple interfaces, one for video and one for audio
    find_card_id_recursive(usb_device)
}

#[cfg(target_os = "linux")]
fn find_card_id_recursive(path: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(path).ok()?;
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        // Look for directories named "card*" under a "sound" directory
        if file_name == "sound" {
            if let Ok(sound_entries) = std::fs::read_dir(&entry_path) {
                for sound_entry in sound_entries.flatten() {
                    let card_name = sound_entry.file_name().to_string_lossy().to_string();
                    if card_name.starts_with("card") {
                        let id_path = sound_entry.path().join("id");
                        if let Ok(id) = std::fs::read_to_string(&id_path) {
                            let id = id.trim().to_string();
                            if !id.is_empty() {
                                return Some(id);
                            }
                        }
                    }
                }
            }
        }

        // Recurse into subdirectories (but not symlinks to avoid loops)
        if entry_path.is_dir() && !entry_path.is_symlink() {
            if let Some(id) = find_card_id_recursive(&entry_path) {
                return Some(id);
            }
        }
    }
    None
}

/// Checks if a sysfs video4linux device belongs to the ShadowCast USB device
/// by walking up the device tree to find the USB vendor/product IDs.
#[cfg(target_os = "linux")]
fn is_shadowcast_usb_device(sysfs_path: &std::path::Path) -> bool {
    // Resolve the "device" symlink to find the parent USB device
    let device_link = sysfs_path.join("device");
    let resolved = match std::fs::canonicalize(&device_link) {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Walk up looking for idVendor/idProduct files
    let mut current = resolved.as_path();
    for _ in 0..10 {
        let vendor_path = current.join("idVendor");
        let product_path = current.join("idProduct");

        if vendor_path.exists() && product_path.exists() {
            let vendor = std::fs::read_to_string(&vendor_path)
                .unwrap_or_default()
                .trim()
                .to_string();
            let product = std::fs::read_to_string(&product_path)
                .unwrap_or_default()
                .trim()
                .to_string();

            return vendor == SHADOWCAST_VENDOR_ID && product == SHADOWCAST_PRODUCT_ID;
        }

        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
    }

    false
}

// --- macOS implementation ---

#[cfg(target_os = "macos")]
fn find_shadowcast_macos() -> Option<CaptureDevice> {
    enumerate_devices_macos()
        .into_iter()
        .find(|d| d.name.to_lowercase().contains("shadowcast"))
}

#[cfg(target_os = "macos")]
fn enumerate_devices_macos() -> Vec<CaptureDevice> {
    use objc2_av_foundation::{
        AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition,
        AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeExternal, AVMediaTypeVideo,
    };
    use objc2_foundation::NSArray;

    let mut devices = Vec::new();

    let media_type = unsafe { AVMediaTypeVideo };
    let Some(media_type) = media_type else {
        return devices;
    };

    let device_types = unsafe {
        NSArray::from_slice(&[
            AVCaptureDeviceTypeBuiltInWideAngleCamera as &objc2_foundation::NSString,
            AVCaptureDeviceTypeExternal as &objc2_foundation::NSString,
        ])
    };
    let session = unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &device_types,
            Some(media_type),
            AVCaptureDevicePosition::Unspecified,
        )
    };
    let av_devices = unsafe { session.devices() };

    for av_device in av_devices.iter() {
        let name = unsafe { av_device.localizedName() }.to_string();
        let unique_id = unsafe { av_device.uniqueID() }.to_string();

        // On macOS, CoreAudio device names typically match the product name
        let audio_matches = vec![name.clone()];

        devices.push(CaptureDevice {
            name,
            video_path: unique_id,
            audio_matches,
        });
    }

    devices
}

// --- Windows implementation ---

#[cfg(target_os = "windows")]
fn find_shadowcast_windows() -> Option<CaptureDevice> {
    enumerate_devices_windows()
        .into_iter()
        .find(|d| d.name.to_lowercase().contains("shadowcast"))
}

#[cfg(target_os = "windows")]
fn enumerate_devices_windows() -> Vec<CaptureDevice> {
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, MFCreateAttributes, MFEnumDeviceSources, MFShutdown, MFStartup,
        MFSTARTUP_NOSOCKET, MF_API_VERSION, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    let mut devices = Vec::new();

    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return devices;
        }
        if MFStartup(MF_API_VERSION, MFSTARTUP_NOSOCKET).is_err() {
            CoUninitialize();
            return devices;
        }

        let result = (|| -> Option<Vec<CaptureDevice>> {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1).ok()?;
            let attrs = attrs?;

            attrs
                .SetGUID(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
                )
                .ok()?;

            let mut devices_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count: u32 = 0;
            MFEnumDeviceSources(&attrs, &mut devices_ptr, &mut count).ok()?;

            if count == 0 || devices_ptr.is_null() {
                return Some(Vec::new());
            }

            let device_slice = std::slice::from_raw_parts(devices_ptr, count as usize);
            let mut result = Vec::new();

            for device_opt in device_slice {
                let Some(device) = device_opt else { continue };

                let mut name_pwstr = windows::core::PWSTR::null();
                let mut name_len: u32 = 0;
                if device
                    .GetAllocatedString(
                        &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
                        &mut name_pwstr,
                        &mut name_len,
                    )
                    .is_ok()
                {
                    let name = name_pwstr.to_string().unwrap_or_default();
                    windows::Win32::System::Com::CoTaskMemFree(Some(
                        name_pwstr.as_ptr() as *const _
                    ));

                    // On Windows, WASAPI device names typically match the product name
                    let audio_matches = vec![name.clone()];

                    result.push(CaptureDevice {
                        name,
                        video_path: String::new(), // MF uses activation objects, not paths
                        audio_matches,
                    });
                }
            }

            windows::Win32::System::Com::CoTaskMemFree(Some(devices_ptr as *const _));
            Some(result)
        })();

        MFShutdown().ok();
        CoUninitialize();

        if let Some(devs) = result {
            devices = devs;
        }
    }

    devices
}
