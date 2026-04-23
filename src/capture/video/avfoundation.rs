use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError, TrySendError};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, Message};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureSessionPresetInputPriority, AVCaptureVideoDataOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};

use crate::capture::format::{CaptureFormat, Frame, FramePixelFormat, PixelFormat};
use crate::stats::FrameStats;

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
    _output: Retained<AVCaptureVideoDataOutput>,
    frame_rx: Receiver<RawFrame>,
    _delegate: Retained<FrameDelegate>,
    current_format: Option<CaptureFormat>,
    stats: Arc<FrameStats>,
}

// The delegate object that receives frames from AVCaptureVideoDataOutput.
// We use define_class! to create an Objective-C class in Rust that implements
// the AVCaptureVideoDataOutputSampleBufferDelegate protocol.
struct FrameDelegateIvars {
    sender: Sender<RawFrame>,
    stats: Arc<FrameStats>,
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
                // Pass the pixel data through as BGRA. wgpu uploads it to a
                // Bgra8UnormSrgb texture and the GPU handles the channel
                // swizzle — no CPU colorspace conversion.
                let src = std::slice::from_raw_parts(
                    base_address as *const u8,
                    bytes_per_row * height as usize,
                );

                let tight_row = (width as usize) * 4;
                let mut bgra = Vec::with_capacity(tight_row * height as usize);
                if bytes_per_row == tight_row {
                    bgra.extend_from_slice(&src[..tight_row * height as usize]);
                } else {
                    // CVPixelBuffer rows can be padded for alignment; drop the
                    // padding so downstream sees a tightly-packed buffer.
                    for y in 0..height as usize {
                        let src_offset = y * bytes_per_row;
                        bgra.extend_from_slice(&src[src_offset..src_offset + tight_row]);
                    }
                }

                self.ivars().stats.inc_captured();
                match self.ivars().sender.try_send(RawFrame {
                    data: bgra,
                    width,
                    height,
                    timestamp: Instant::now(),
                }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                        self.ivars().stats.inc_dropped_at_capture();
                    }
                }
            }

            CVPixelBufferUnlockBaseAddress(&image_buffer, CVPixelBufferLockFlags(0));
        }
    }
);

impl FrameDelegate {
    fn new(sender: Sender<RawFrame>, stats: Arc<FrameStats>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(FrameDelegateIvars { sender, stats });
        unsafe { msg_send![super(this), init] }
    }
}

impl AvFoundationSource {
    /// Opens a video capture device by path/identifier.
    ///
    /// On macOS, `device_path` is the AVCaptureDevice unique ID string
    /// or a substring to match against the device's localized name.
    pub fn new(device_path: &str, stats: Arc<FrameStats>) -> Result<Self> {
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

        // Tell AVCaptureSession not to pick a format for us. Without this the
        // session defaults to `AVCaptureSessionPresetHigh`, which overrides
        // anything we set via `AVCaptureDevice::setActiveFormat` when
        // `startRunning` runs — the visible symptom is that the device falls
        // back to its native default frame rate (e.g. 25 or 30 fps) even after
        // we've picked a 60 fps device format. Must run after `addInput`:
        // `canSetSessionPreset(InputPriority)` only returns true once at least
        // one input is attached.
        unsafe {
            let input_priority = AVCaptureSessionPresetInputPriority;
            if session.canSetSessionPreset(input_priority) {
                session.setSessionPreset(input_priority);
            } else {
                log::warn!(
                    "Cannot set AVCaptureSession preset to InputPriority; \
                     device activeFormat may be overridden"
                );
            }
        }

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

        // Set up the delegate with a serial dispatch queue. Channel depth of 3
        // gives the render thread a one-frame tolerance for stalls before
        // capture starts dropping, without adding meaningful latency — with
        // the drain-to-latest receive in `try_next_frame`, stale frames are
        // discarded rather than queued up.
        let (frame_tx, frame_rx) = bounded(3);
        let delegate = FrameDelegate::new(frame_tx, stats.clone());

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
            _output: output,
            frame_rx,
            _delegate: delegate,
            current_format: None,
            stats,
        })
    }
}

/// Render a FourCharCode (Core Media subtype) as its printable 4-char tag
/// (e.g. `0x6D6A7067` → `"mjpg"`). Falls back to hex when bytes are unprintable.
fn format_fourcc(fcc: u32) -> String {
    let bytes = fcc.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("0x{:08x}", fcc)
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
        // Walk device formats looking for a match on dimensions AND a frame-
        // rate range that actually tops out at the requested fps. The old
        // code took the first range containing `format.fps`, which — for a
        // device format advertising e.g. 15–120 fps — would then set
        // minFrameDuration to 1/120 instead of 1/60. Prefer a range whose
        // maxFrameRate matches the requested fps (within 0.5 fps to tolerate
        // devices that report fractional rates like 59.9998).
        #[derive(Clone, Copy)]
        enum MatchKind {
            Exact,
            Contained,
        }

        let requested_fps = format.fps as f64;
        let device_formats = unsafe { self.device.formats() };
        let mut target: Option<(
            objc2::rc::Retained<objc2_av_foundation::AVCaptureDeviceFormat>,
            objc2::rc::Retained<objc2_av_foundation::AVFrameRateRange>,
            MatchKind,
        )> = None;
        let mut fallback: Option<(
            objc2::rc::Retained<objc2_av_foundation::AVCaptureDeviceFormat>,
            objc2::rc::Retained<objc2_av_foundation::AVFrameRateRange>,
            MatchKind,
        )> = None;

        for fmt in device_formats.iter() {
            let desc = unsafe { fmt.formatDescription() };
            let dimensions =
                unsafe { objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc) };
            if dimensions.width as u32 != format.width
                || dimensions.height as u32 != format.height
            {
                continue;
            }

            let ranges = unsafe { fmt.videoSupportedFrameRateRanges() };
            for range in ranges.iter() {
                let min_fps = unsafe { range.minFrameRate() };
                let max_fps = unsafe { range.maxFrameRate() };

                if (max_fps - requested_fps).abs() <= 0.5 {
                    target = Some((fmt.retain(), range.retain(), MatchKind::Exact));
                    break;
                }
                if fallback.is_none()
                    && requested_fps >= min_fps - 0.5
                    && requested_fps <= max_fps + 0.5
                {
                    fallback = Some((fmt.retain(), range.retain(), MatchKind::Contained));
                }
            }
            if target.is_some() {
                break;
            }
        }

        let Some((avformat, range, match_kind)) = target.or(fallback) else {
            anyhow::bail!(
                "No matching format found for {}x{} @ {}fps",
                format.width,
                format.height,
                format.fps
            );
        };

        // Diagnostic: what native format are we asking the device for? Useful
        // when the capture rate doesn't match the requested fps — e.g. picking
        // an uncompressed subtype (`420v`, `BGRA`) where USB bandwidth caps at
        // 30 fps even though a sibling `MJPG` format would do 60.
        let subtype = {
            let desc = unsafe { avformat.formatDescription() };
            let fcc = unsafe { desc.media_sub_type() };
            format_fourcc(fcc)
        };
        let range_min = unsafe { range.minFrameRate() };
        let range_max = unsafe { range.maxFrameRate() };
        log::info!(
            "AVFoundation selected device format: {}x{} {} [{:.2}–{:.2} fps] for requested {}x{}@{}",
            format.width,
            format.height,
            subtype,
            range_min,
            range_max,
            format.width,
            format.height,
            format.fps
        );

        // Pick the frame duration to clamp the device to. When we matched a
        // range whose max equals the requested fps, the range's own
        // `minFrameDuration` is the exact right value (and handles rates like
        // 60000240/1000000 that won't round-trip through a hand-computed
        // 1/fps). When we only have a "contained" match — e.g. a 5-120 fps
        // range when asking for 60 — we must compute 1/fps ourselves, since
        // `range.minFrameDuration` would clamp the device to the range's max.
        let duration = match match_kind {
            MatchKind::Exact => unsafe { range.minFrameDuration() },
            MatchKind::Contained => CMTime {
                value: 1,
                timescale: format.fps as i32,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            },
        };

        // Wrap device configuration inside the session's begin/commit so the
        // activeFormat + frame duration changes are applied atomically and
        // the session doesn't re-pick a format between our three setters.
        unsafe {
            self.session.beginConfiguration();

            let lock_result = self
                .device
                .lockForConfiguration()
                .map_err(|e| anyhow::anyhow!("Failed to lock device for configuration: {}", e));
            if let Err(e) = lock_result {
                self.session.commitConfiguration();
                return Err(e);
            }

            self.device.setActiveFormat(&avformat);
            self.device.setActiveVideoMinFrameDuration(duration);
            self.device.setActiveVideoMaxFrameDuration(duration);
            self.device.unlockForConfiguration();

            self.session.commitConfiguration();
        }

        // Verify: read back the activeFormat + active frame duration so we
        // can confirm the change stuck. If AVFoundation ignored us (e.g.
        // because the preset silently reverted), this log line will show
        // a mismatch.
        unsafe {
            let active = self.device.activeFormat();
            let desc = active.formatDescription();
            let dims = objc2_core_media::CMVideoFormatDescriptionGetDimensions(&desc);
            let active_subtype = format_fourcc(desc.media_sub_type());
            let min_dur: CMTime = self.device.activeVideoMinFrameDuration();
            let effective_fps = if min_dur.value > 0 {
                min_dur.timescale as f64 / min_dur.value as f64
            } else {
                0.0
            };
            log::info!(
                "AVFoundation active format after set: {}x{} {} @ {:.2} fps",
                dims.width,
                dims.height,
                active_subtype,
                effective_fps
            );
        }

        self.current_format = Some(format.clone());
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        unsafe { self.session.startRunning() };
        Ok(())
    }

    fn try_next_frame(&mut self) -> Result<Option<Frame>> {
        // Drain any queued frames and keep only the newest. Older frames are
        // already stale by the time the render loop sees them; rendering them
        // would just push user-visible latency further from the live signal.
        let mut latest: Option<RawFrame> = None;
        let mut dropped = 0u64;
        loop {
            match self.frame_rx.try_recv() {
                Ok(raw) => {
                    if latest.is_some() {
                        dropped += 1;
                    }
                    latest = Some(raw);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("Video capture channel closed");
                }
            }
        }
        if dropped > 0 {
            self.stats.add_dropped_at_render(dropped);
        }
        Ok(latest.map(|raw| Frame {
            width: raw.width,
            height: raw.height,
            data: Arc::new(raw.data),
            pixel_format: FramePixelFormat::Bgra8,
            timestamp: raw.timestamp,
        }))
    }

    fn stop(&mut self) -> Result<()> {
        unsafe { self.session.stopRunning() };
        Ok(())
    }
}
