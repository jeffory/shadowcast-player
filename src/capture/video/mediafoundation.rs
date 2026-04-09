use std::time::Instant;

use anyhow::{Context, Result};
use windows::core::GUID;
use windows::Win32::Foundation::FALSE;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSourceReader, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFShutdown,
    MFStartup, MFSTARTUP_NOSOCKET, MF_API_VERSION, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::capture::format::{CaptureFormat, Frame, PixelFormat};

use super::VideoSource;

/// MFVideoFormat_MJPG GUID
const MF_VIDEO_FORMAT_MJPG: GUID = GUID::from_u128(0x47504a4d_0000_0010_8000_00aa00389b71);
/// MFVideoFormat_YUY2 GUID
const MF_VIDEO_FORMAT_YUY2: GUID = GUID::from_u128(0x32595559_0000_0010_8000_00aa00389b71);
/// MFVideoFormat_NV12 GUID
const MF_VIDEO_FORMAT_NV12: GUID = GUID::from_u128(0x3231564e_0000_0010_8000_00aa00389b71);
/// MFVideoFormat_RGB24 GUID
const MF_VIDEO_FORMAT_RGB24: GUID = GUID::from_u128(0x00000014_0000_0010_8000_00aa00389b71);

/// Media Foundation capture backend for Windows.
pub struct MediaFoundationSource {
    reader: IMFSourceReader,
    current_format: Option<CaptureFormat>,
    _com_initialized: ComGuard,
}

/// RAII guard for COM initialization/shutdown.
struct ComGuard;

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            MFShutdown().ok();
            CoUninitialize();
        }
    }
}

fn init_com() -> Result<ComGuard> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .context("Failed to initialize COM")?;
        MFStartup(MF_API_VERSION, MFSTARTUP_NOSOCKET)
            .context("Failed to initialize Media Foundation")?;
    }
    Ok(ComGuard)
}

impl MediaFoundationSource {
    /// Opens a video capture device by name or path.
    ///
    /// On Windows, `device_path` is matched as a substring against
    /// the friendly name of enumerated video capture devices.
    pub fn new(device_path: &str) -> Result<Self> {
        let com_guard = init_com()?;

        let source = find_device(device_path)?;

        let reader = unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.context("Failed to create attributes")?;

            // Disable automatic format conversion so we get native formats
            attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, FALSE.0 as u32)?;

            MFCreateSourceReaderFromMediaSource(&source, &attrs)?
        };

        Ok(Self {
            reader,
            current_format: None,
            _com_initialized: com_guard,
        })
    }
}

/// Enumerate video capture devices and find one matching the given name.
fn find_device(name: &str) -> Result<IMFMediaSource> {
    let search = name.to_lowercase();

    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("Failed to create attributes for device enumeration")?;

        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;

        let mut devices_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        MFEnumDeviceSources(&attrs, &mut devices_ptr, &mut count)?;

        if count == 0 || devices_ptr.is_null() {
            anyhow::bail!("No video capture devices found");
        }

        // Wrap in a slice for safe iteration
        let devices = std::slice::from_raw_parts(devices_ptr, count as usize);

        let mut matched_device = None;

        for device_opt in devices {
            let Some(device) = device_opt else { continue };

            // Get the friendly name
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
                let device_name = name_pwstr.to_string().unwrap_or_default();
                log::debug!("Found video device: '{}'", device_name);

                if device_name.to_lowercase().contains(&search) {
                    matched_device = Some(device.clone());
                    windows::Win32::System::Com::CoTaskMemFree(Some(
                        name_pwstr.as_ptr() as *const _
                    ));
                    break;
                }

                windows::Win32::System::Com::CoTaskMemFree(Some(name_pwstr.as_ptr() as *const _));
            }
        }

        // Free the device array
        windows::Win32::System::Com::CoTaskMemFree(Some(devices_ptr as *const _));

        let device =
            matched_device.context(format!("No video device found matching '{}'", name))?;

        // Activate the device to get the media source
        let source: IMFMediaSource = device.ActivateObject()?;
        Ok(source)
    }
}

/// Extract width and height from a packed MF_MT_FRAME_SIZE value.
fn unpack_frame_size(packed: u64) -> (u32, u32) {
    let width = (packed >> 32) as u32;
    let height = packed as u32;
    Ok::<_, ()>((width, height)).unwrap()
}

/// Extract fps from a packed MF_MT_FRAME_RATE value.
fn unpack_frame_rate(packed: u64) -> u32 {
    let numerator = (packed >> 32) as u32;
    let denominator = packed as u32;
    if denominator > 0 {
        numerator / denominator
    } else {
        0
    }
}

/// Map a Media Foundation subtype GUID to our PixelFormat.
fn guid_to_pixel_format(guid: &GUID) -> Option<PixelFormat> {
    if *guid == MF_VIDEO_FORMAT_MJPG {
        Some(PixelFormat::Mjpeg)
    } else if *guid == MF_VIDEO_FORMAT_YUY2 {
        Some(PixelFormat::Yuyv)
    } else {
        None
    }
}

impl VideoSource for MediaFoundationSource {
    fn supported_formats(&self) -> Vec<CaptureFormat> {
        let mut formats = Vec::new();

        unsafe {
            let mut index: u32 = 0;
            loop {
                let media_type: Result<IMFMediaType, _> = self
                    .reader
                    .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, index);

                let Ok(media_type) = media_type else { break };

                // Check this is a video type
                let Ok(major_type) = media_type.GetGUID(&MF_MT_MAJOR_TYPE) else {
                    index += 1;
                    continue;
                };
                if major_type != MFMediaType_Video {
                    index += 1;
                    continue;
                }

                // Get subtype (pixel format)
                let Ok(subtype) = media_type.GetGUID(&MF_MT_SUBTYPE) else {
                    index += 1;
                    continue;
                };
                let Some(pixel_format) = guid_to_pixel_format(&subtype) else {
                    index += 1;
                    continue;
                };

                // Get frame size
                let Ok(frame_size) = media_type.GetUINT64(&MF_MT_FRAME_SIZE) else {
                    index += 1;
                    continue;
                };
                let (width, height) = unpack_frame_size(frame_size);

                // Get frame rate
                let Ok(frame_rate) = media_type.GetUINT64(&MF_MT_FRAME_RATE) else {
                    index += 1;
                    continue;
                };
                let fps = unpack_frame_rate(frame_rate);

                if width > 0 && height > 0 && fps > 0 {
                    formats.push(CaptureFormat {
                        width,
                        height,
                        fps,
                        pixel_format,
                    });
                }

                index += 1;
            }
        }

        // Sort by resolution (highest first), then fps (highest first)
        formats.sort_by(|a, b| {
            let res_a = a.width * a.height;
            let res_b = b.width * b.height;
            res_b.cmp(&res_a).then(b.fps.cmp(&a.fps))
        });

        formats
    }

    fn set_format(&mut self, format: &CaptureFormat) -> Result<()> {
        unsafe {
            // Find the matching native media type
            let mut index: u32 = 0;
            loop {
                let media_type: Result<IMFMediaType, _> = self
                    .reader
                    .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, index);

                let Ok(media_type) = media_type else {
                    anyhow::bail!(
                        "No matching format found for {}x{} @ {}fps",
                        format.width,
                        format.height,
                        format.fps
                    );
                };

                let Ok(subtype) = media_type.GetGUID(&MF_MT_SUBTYPE) else {
                    index += 1;
                    continue;
                };

                if guid_to_pixel_format(&subtype).as_ref() != Some(&format.pixel_format) {
                    index += 1;
                    continue;
                }

                let Ok(frame_size) = media_type.GetUINT64(&MF_MT_FRAME_SIZE) else {
                    index += 1;
                    continue;
                };
                let (width, height) = unpack_frame_size(frame_size);

                let Ok(frame_rate) = media_type.GetUINT64(&MF_MT_FRAME_RATE) else {
                    index += 1;
                    continue;
                };
                let fps = unpack_frame_rate(frame_rate);

                if width == format.width && height == format.height && fps == format.fps {
                    self.reader.SetCurrentMediaType(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        None,
                        &media_type,
                    )?;
                    self.current_format = Some(format.clone());
                    return Ok(());
                }

                index += 1;
            }
        }
    }

    fn start(&mut self) -> Result<()> {
        // Media Foundation's source reader starts delivering samples on ReadSample,
        // no explicit start needed.
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Frame> {
        let current_format = self
            .current_format
            .as_ref()
            .context("No format set; call set_format() before capturing")?;

        unsafe {
            let mut flags: u32 = 0;
            let mut _timestamp: i64 = 0;
            let mut _actual_index: u32 = 0;
            let mut sample = None;

            self.reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                Some(&mut _actual_index),
                Some(&mut flags),
                Some(&mut _timestamp),
                Some(&mut sample),
            )?;

            let sample = sample.context("ReadSample returned no sample")?;
            let buffer = sample.ConvertToContiguousBuffer()?;

            let mut buf_ptr: *mut u8 = std::ptr::null_mut();
            let mut _max_len: u32 = 0;
            let mut cur_len: u32 = 0;
            buffer.Lock(&mut buf_ptr, Some(&mut _max_len), Some(&mut cur_len))?;

            let raw_data = std::slice::from_raw_parts(buf_ptr, cur_len as usize).to_vec();

            buffer.Unlock()?;

            // Decode based on pixel format
            let (data, width, height) = match current_format.pixel_format {
                PixelFormat::Mjpeg => crate::capture::format::mjpeg_to_rgb(&raw_data)?,
                PixelFormat::Yuyv => {
                    let w = current_format.width;
                    let h = current_format.height;
                    let rgb = crate::capture::format::yuyv_to_rgb(&raw_data, w, h);
                    (rgb, w, h)
                }
            };

            Ok(Frame {
                width,
                height,
                data,
                timestamp: Instant::now(),
            })
        }
    }

    fn stop(&mut self) -> Result<()> {
        // Flush the source reader to stop delivering samples
        unsafe {
            self.reader
                .Flush(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .ok();
        }
        Ok(())
    }
}
