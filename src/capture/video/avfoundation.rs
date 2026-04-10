use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};

use crate::capture::format::{CaptureFormat, Frame, PixelFormat};

use super::VideoSource;

/// Frame data sent from the delegate callback to the consumer.
struct RawFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
    timestamp: Instant,
}

/// AVFoundation capture backend for macOS.
pub struct AvFoundationSource {
    device: Retained<AVCaptureDevice>,
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    frame_rx: Receiver<RawFrame>,
    _delegate: Retained<FrameDelegate>,
    current_format: Option<CaptureFormat>,
}

// The delegate object that receives frames from AVCaptureVideoDataOutput.
// We use define_class! to create an Objective-C class in Rust that implements
// the AVCaptureVideoDataOutputSampleBufferDelegate protocol.
struct FrameDelegateIvars {
    sender: Sender<RawFrame>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[name = "GenkiFrameDelegate"]
    #[ivars = FrameDelegateIvars]
    struct FrameDelegate;

    unsafe impl NSObjectProtocol for FrameDelegate {}

    #[allow(non_snake_case)]
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn captureOutput_didOutputSampleBuffer_fromConnection(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let Some(image_buffer) = sample_buffer.image_buffer() else {
                return;
            };

            // Lock the pixel buffer base address for reading
            CVPixelBufferLockBaseAddress(&image_buffer, CVPixelBufferLockFlags(0));

            let width = CVPixelBufferGetWidth(&image_buffer) as u32;
            let height = CVPixelBufferGetHeight(&image_buffer) as u32;
            let bytes_per_row = CVPixelBufferGetBytesPerRow(&image_buffer);
            let base_address = CVPixelBufferGetBaseAddress(&image_buffer);

            if !base_address.is_null() && width > 0 && height > 0 {
                // Convert BGRA to RGB
                let src = std::slice::from_raw_parts(
                    base_address as *const u8,
                    bytes_per_row * height as usize,
                );

                let mut rgb = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let offset = y * bytes_per_row + x * 4;
                        rgb.push(src[offset + 2]); // R (from BGRA)
                        rgb.push(src[offset + 1]); // G
                        rgb.push(src[offset]); // B
                    }
                }

                let _ = self.ivars().sender.try_send(RawFrame {
                    data: rgb,
                    width,
                    height,
                    timestamp: Instant::now(),
                });
            }

            CVPixelBufferUnlockBaseAddress(&image_buffer, CVPixelBufferLockFlags(0));
        }
    }
);

impl FrameDelegate {
    fn new(sender: Sender<RawFrame>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(FrameDelegateIvars { sender });
        unsafe { msg_send![super(this), init] }
    }
}

impl AvFoundationSource {
    /// Opens a video capture device by path/identifier.
    ///
    /// On macOS, `device_path` is the AVCaptureDevice unique ID string
    /// or a substring to match against the device's localized name.
    pub fn new(device_path: &str) -> Result<Self> {
        let device = find_device(device_path).context(format!(
            "No video capture device found matching '{}'",
            device_path
        ))?;

        let session = unsafe { AVCaptureSession::new() };

        // Create input from device
        let input = unsafe {
            AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| anyhow::anyhow!("Failed to create device input: {}", e))?
        };

        if unsafe { !session.canAddInput(&input) } {
            anyhow::bail!("Cannot add input to capture session");
        }
        unsafe { session.addInput(&input) };

        // Create video data output configured for BGRA pixel format
        let output = unsafe { AVCaptureVideoDataOutput::new() };

        // Set pixel format to BGRA for efficient conversion.
        // The key is kCVPixelBufferPixelFormatTypeKey ("PixelFormatType").
        let format_key = NSString::from_str("PixelFormatType");
        let format_value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
        let settings: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_slices(&[&*format_key], &[format_value.as_ref()]);
        unsafe { output.setVideoSettings(Some(&settings)) };
        unsafe { output.setAlwaysDiscardsLateVideoFrames(true) };

        // Set up the delegate with a serial dispatch queue
        let (frame_tx, frame_rx) = bounded(2);
        let delegate = FrameDelegate::new(frame_tx);

        let queue = dispatch2::DispatchQueue::new("com.shadowcast-player.video-capture", None);
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
        }

        if unsafe { !session.canAddOutput(&output) } {
            anyhow::bail!("Cannot add output to capture session");
        }
        unsafe { session.addOutput(&output) };

        Ok(Self {
            device,
            session,
            output,
            frame_rx,
            _delegate: delegate,
            current_format: None,
        })
    }
}

/// Find an AVCaptureDevice matching the given path or name substring.
fn find_device(name_or_id: &str) -> Option<Retained<AVCaptureDevice>> {
    // First try exact unique ID match
    let ns_id = NSString::from_str(name_or_id);
    if let Some(device) = unsafe { AVCaptureDevice::deviceWithUniqueID(&ns_id) } {
        return Some(device);
    }

    // Fall back to name substring match
    let search = name_or_id.to_lowercase();
    let media_type = unsafe { AVMediaTypeVideo }?;

    use objc2_av_foundation::{
        AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition,
        AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeExternal,
    };
    use objc2_foundation::NSArray;

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
    let devices = unsafe { session.devices() };

    for device in devices.iter() {
        let name = unsafe { device.localizedName() }.to_string();
        if name.to_lowercase().contains(&search) {
            return Some(device.clone());
        }
    }

    None
}

impl VideoSource for AvFoundationSource {
    fn supported_formats(&self) -> Vec<CaptureFormat> {
        let mut formats = Vec::new();

        let device_formats = unsafe { self.device.formats() };
        for fmt in device_formats.iter() {
            let desc = unsafe { fmt.formatDescription() };
            let dimensions =
                unsafe { objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc) };

            let width = dimensions.width as u32;
            let height = dimensions.height as u32;

            // Get supported frame rate ranges
            let ranges = unsafe { fmt.videoSupportedFrameRateRanges() };
            for range in ranges.iter() {
                let max_fps = unsafe { range.maxFrameRate() } as u32;
                if max_fps > 0 {
                    // AVFoundation delivers decoded frames, so we treat them as MJPEG
                    // since our format system expects a pixel format designation
                    formats.push(CaptureFormat {
                        width,
                        height,
                        fps: max_fps,
                        pixel_format: PixelFormat::Mjpeg,
                    });
                }
            }
        }

        // Sort by resolution (highest first), then fps (highest first)
        formats.sort_by(|a, b| {
            let res_a = a.width * a.height;
            let res_b = b.width * b.height;
            res_b.cmp(&res_a).then(b.fps.cmp(&a.fps))
        });

        // Deduplicate
        formats.dedup();

        formats
    }

    fn set_format(&mut self, format: &CaptureFormat) -> Result<()> {
        // Find the matching AVCaptureDeviceFormat
        let device_formats = unsafe { self.device.formats() };
        let target_format = device_formats.iter().find(|fmt| {
            let desc = unsafe { fmt.formatDescription() };
            let dimensions =
                unsafe { objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc) };
            let width = dimensions.width as u32;
            let height = dimensions.height as u32;

            if width != format.width || height != format.height {
                return false;
            }

            let ranges = unsafe { fmt.videoSupportedFrameRateRanges() };
            ranges.iter().any(|range| {
                let max_fps = unsafe { range.maxFrameRate() } as u32;
                max_fps >= format.fps
            })
        });

        let Some(avformat) = target_format else {
            anyhow::bail!(
                "No matching format found for {}x{} @ {}fps",
                format.width,
                format.height,
                format.fps
            );
        };

        unsafe {
            self.device
                .lockForConfiguration()
                .map_err(|e| anyhow::anyhow!("Failed to lock device for configuration: {}", e))?;

            self.device.setActiveFormat(&avformat);

            // Set frame duration to 1/fps
            let duration = objc2_core_media::CMTime {
                value: 1,
                timescale: format.fps as i32,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            };
            self.device.setActiveVideoMinFrameDuration(duration);
            self.device.setActiveVideoMaxFrameDuration(duration);

            self.device.unlockForConfiguration();
        }

        self.current_format = Some(format.clone());
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        unsafe { self.session.startRunning() };
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Frame> {
        let raw = self
            .frame_rx
            .recv()
            .context("Video capture channel closed")?;

        Ok(Frame {
            width: raw.width,
            height: raw.height,
            data: raw.data,
            timestamp: raw.timestamp,
        })
    }

    fn stop(&mut self) -> Result<()> {
        unsafe { self.session.stopRunning() };
        Ok(())
    }
}
