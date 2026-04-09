use std::time::Instant;

use anyhow::{Context, Result};
use v4l::buffer::Type as BufType;
use v4l::format::{Format as V4lFormat, FourCC};
use v4l::frameinterval::FrameIntervalEnum;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::Device;

use crate::capture::format::{mjpeg_to_rgb, yuyv_to_rgb, CaptureFormat, Frame, PixelFormat};

use super::VideoSource;

/// V4L2 capture backend using mmap streaming.
pub struct V4l2Source {
    device: Device,
    stream: Option<Stream<'static>>,
    current_format: Option<CaptureFormat>,
}

impl V4l2Source {
    /// Opens a V4L2 device at the given path (e.g. "/dev/video2").
    pub fn new(device_path: &str) -> Result<Self> {
        let device = Device::with_path(device_path).context("Failed to open V4L2 device")?;
        Ok(Self {
            device,
            stream: None,
            current_format: None,
        })
    }
}

/// Maps a V4L2 FourCC to our PixelFormat, returning None for unsupported formats.
fn fourcc_to_pixel_format(fourcc: &FourCC) -> Option<PixelFormat> {
    if *fourcc == FourCC::new(b"MJPG") {
        Some(PixelFormat::Mjpeg)
    } else if *fourcc == FourCC::new(b"YUYV") {
        Some(PixelFormat::Yuyv)
    } else {
        None
    }
}

impl VideoSource for V4l2Source {
    fn supported_formats(&self) -> Vec<CaptureFormat> {
        let mut formats = Vec::new();

        let descriptions = match self.device.enum_formats() {
            Ok(d) => d,
            Err(_) => return formats,
        };

        for desc in &descriptions {
            let pixel_format = match fourcc_to_pixel_format(&desc.fourcc) {
                Some(pf) => pf,
                None => continue, // Skip unsupported formats
            };

            let framesizes = match self.device.enum_framesizes(desc.fourcc) {
                Ok(fs) => fs,
                Err(_) => continue,
            };

            for framesize in framesizes {
                let discretes: Vec<_> = framesize.size.to_discrete().into_iter().collect();
                for discrete in discretes {
                    let intervals = match self.device.enum_frameintervals(
                        desc.fourcc,
                        discrete.width,
                        discrete.height,
                    ) {
                        Ok(fi) => fi,
                        Err(_) => continue,
                    };

                    for interval in intervals {
                        match interval.interval {
                            FrameIntervalEnum::Discrete(frac) => {
                                if frac.numerator > 0 {
                                    let fps = frac.denominator / frac.numerator;
                                    formats.push(CaptureFormat {
                                        width: discrete.width,
                                        height: discrete.height,
                                        fps,
                                        pixel_format,
                                    });
                                }
                            }
                            FrameIntervalEnum::Stepwise(_) => {
                                // Stepwise intervals not enumerated individually
                            }
                        }
                    }
                }
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
        let fourcc = match format.pixel_format {
            PixelFormat::Mjpeg => FourCC::new(b"MJPG"),
            PixelFormat::Yuyv => FourCC::new(b"YUYV"),
        };

        let v4l_fmt = V4lFormat::new(format.width, format.height, fourcc);
        Capture::set_format(&self.device, &v4l_fmt).context("Failed to set V4L2 format")?;

        // Set the frame interval (1/fps) to control capture rate
        let params = v4l::video::capture::Parameters::with_fps(format.fps);
        Capture::set_params(&self.device, &params).context("Failed to set V4L2 frame interval")?;

        self.current_format = Some(format.clone());
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        // Safety: The Stream borrows the Device, but both are owned by V4l2Source.
        // The Device will not be dropped or moved while the Stream exists because
        // stop() drops the Stream before the Device can be dropped (V4l2Source's
        // drop order drops fields in declaration order: device after stream would
        // be wrong, but we ensure stream is dropped first via stop()). We also
        // always drop the stream in stop() before any operation that could
        // invalidate the device. The 'static lifetime is safe because we guarantee
        // the Device outlives the Stream through the V4l2Source ownership.
        let stream = unsafe {
            let device_ptr: *const Device = &self.device;
            Stream::with_buffers(&*device_ptr, BufType::VideoCapture, 4)
                .context("Failed to create mmap stream")?
        };

        // Transmute the stream lifetime from the device borrow to 'static.
        // Safety: same reasoning as above -- V4l2Source owns both, and we
        // always drop the stream before the device.
        let stream: Stream<'static> = unsafe { std::mem::transmute(stream) };

        self.stream = Some(stream);
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Frame> {
        let current_format = self
            .current_format
            .as_ref()
            .context("No format set; call set_format() before capturing")?;

        let stream = self
            .stream
            .as_mut()
            .context("Stream not started; call start() before capturing")?;

        let (buf, _meta) =
            CaptureStream::next(stream).context("Failed to capture frame from V4L2 stream")?;

        let (data, width, height) = match current_format.pixel_format {
            PixelFormat::Mjpeg => mjpeg_to_rgb(buf)?,
            PixelFormat::Yuyv => {
                let w = current_format.width;
                let h = current_format.height;
                let rgb = yuyv_to_rgb(buf, w, h);
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

    fn stop(&mut self) -> Result<()> {
        // Drop the stream to stop capture and release mmap buffers.
        // This must happen before the Device is dropped.
        self.stream = None;
        Ok(())
    }
}

impl Drop for V4l2Source {
    fn drop(&mut self) {
        // Ensure the stream is dropped before the device
        self.stream = None;
    }
}
