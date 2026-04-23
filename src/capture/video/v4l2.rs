use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError, TrySendError};
use v4l::buffer::Type as BufType;
use v4l::format::{Format as V4lFormat, FourCC};
use v4l::frameinterval::FrameIntervalEnum;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::Device;

use crate::capture::format::{
    mjpeg_to_rgb, yuyv_to_rgb, CaptureFormat, Frame, FramePixelFormat, PixelFormat,
};
use crate::stats::FrameStats;

use super::VideoSource;

/// Decoded frame handed from the worker thread to the render thread.
struct RawFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
    timestamp: Instant,
}

struct WorkerHandle {
    thread: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    frame_rx: Receiver<RawFrame>,
}

impl WorkerHandle {
    fn shutdown(mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// V4L2 capture backend using mmap streaming.
///
/// Capture + MJPEG/YUYV decode run on a dedicated worker thread so the render
/// loop never blocks on `ioctl(DQBUF)` or CPU-bound decode. Frames cross to
/// the render thread via a bounded channel (drain-to-latest on consumption).
pub struct V4l2Source {
    device_path: String,
    device: Option<Device>,
    current_format: Option<CaptureFormat>,
    stats: Arc<FrameStats>,
    worker: Option<WorkerHandle>,
}

impl V4l2Source {
    /// Opens a V4L2 device at the given path (e.g. "/dev/video2").
    pub fn new(device_path: &str, stats: Arc<FrameStats>) -> Result<Self> {
        let device = Device::with_path(device_path).context("Failed to open V4L2 device")?;
        Ok(Self {
            device_path: device_path.to_string(),
            device: Some(device),
            current_format: None,
            stats,
            worker: None,
        })
    }

    fn ensure_device(&mut self) -> Result<&Device> {
        if self.device.is_none() {
            self.device = Some(
                Device::with_path(&self.device_path).context("Failed to reopen V4L2 device")?,
            );
        }
        Ok(self.device.as_ref().unwrap())
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

fn worker_loop(
    device: Device,
    format: CaptureFormat,
    sender: Sender<RawFrame>,
    stop_flag: Arc<AtomicBool>,
    stats: Arc<FrameStats>,
) {
    let mut stream = match Stream::with_buffers(&device, BufType::VideoCapture, 4) {
        Ok(s) => s,
        Err(e) => {
            log::error!("v4l2 worker: failed to create mmap stream: {}", e);
            return;
        }
    };

    while !stop_flag.load(Ordering::Relaxed) {
        let (buf, _meta) = match CaptureStream::next(&mut stream) {
            Ok(pair) => pair,
            Err(e) => {
                log::error!("v4l2 worker: capture error: {}", e);
                break;
            }
        };

        let frame = match format.pixel_format {
            PixelFormat::Mjpeg => match mjpeg_to_rgb(buf) {
                Ok((data, width, height)) => RawFrame {
                    data,
                    width,
                    height,
                    timestamp: Instant::now(),
                },
                Err(e) => {
                    log::warn!("v4l2 worker: JPEG decode failed: {}", e);
                    continue;
                }
            },
            PixelFormat::Yuyv => {
                let data = yuyv_to_rgb(buf, format.width, format.height);
                RawFrame {
                    data,
                    width: format.width,
                    height: format.height,
                    timestamp: Instant::now(),
                }
            }
        };

        stats.inc_captured();
        match sender.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                stats.inc_dropped_at_capture();
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
}

impl VideoSource for V4l2Source {
    fn supported_formats(&self) -> Vec<CaptureFormat> {
        let mut formats = Vec::new();

        let Some(device) = self.device.as_ref() else {
            return formats;
        };

        let descriptions = match device.enum_formats() {
            Ok(d) => d,
            Err(_) => return formats,
        };

        for desc in &descriptions {
            let pixel_format = match fourcc_to_pixel_format(&desc.fourcc) {
                Some(pf) => pf,
                None => continue,
            };

            let framesizes = match device.enum_framesizes(desc.fourcc) {
                Ok(fs) => fs,
                Err(_) => continue,
            };

            for framesize in framesizes {
                let discretes: Vec<_> = framesize.size.to_discrete().into_iter().collect();
                for discrete in discretes {
                    let intervals = match device.enum_frameintervals(
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

        formats.sort_by(|a, b| {
            let res_a = a.width * a.height;
            let res_b = b.width * b.height;
            res_b.cmp(&res_a).then(b.fps.cmp(&a.fps))
        });

        formats
    }

    fn set_format(&mut self, format: &CaptureFormat) -> Result<()> {
        if self.worker.is_some() {
            anyhow::bail!("Cannot set_format while capture is running; call stop() first");
        }

        let device = self.ensure_device()?;

        let fourcc = match format.pixel_format {
            PixelFormat::Mjpeg => FourCC::new(b"MJPG"),
            PixelFormat::Yuyv => FourCC::new(b"YUYV"),
        };

        let v4l_fmt = V4lFormat::new(format.width, format.height, fourcc);
        Capture::set_format(device, &v4l_fmt).context("Failed to set V4L2 format")?;

        let params = v4l::video::capture::Parameters::with_fps(format.fps);
        Capture::set_params(device, &params).context("Failed to set V4L2 frame interval")?;

        self.current_format = Some(format.clone());
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }

        let format = self
            .current_format
            .clone()
            .context("No format set; call set_format() before capturing")?;

        self.ensure_device()?;
        let device = self
            .device
            .take()
            .expect("ensure_device populated self.device");

        // Channel depth matches the AVFoundation backend: one-frame tolerance
        // for render stalls before capture drops, with drain-to-latest on the
        // receiving side meaning stale frames don't accumulate.
        let (frame_tx, frame_rx) = bounded(3);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop_flag);
        let worker_stats = Arc::clone(&self.stats);

        let thread = std::thread::Builder::new()
            .name("v4l2-capture".into())
            .spawn(move || worker_loop(device, format, frame_tx, worker_stop, worker_stats))
            .context("Failed to spawn v4l2 capture thread")?;

        self.worker = Some(WorkerHandle {
            thread: Some(thread),
            stop_flag,
            frame_rx,
        });
        Ok(())
    }

    fn try_next_frame(&mut self) -> Result<Option<Frame>> {
        let Some(worker) = self.worker.as_ref() else {
            anyhow::bail!("Stream not started; call start() before capturing");
        };

        // Drain-to-latest: stale frames are user-visible latency, so discard
        // all but the newest currently buffered.
        let mut latest: Option<RawFrame> = None;
        let mut dropped = 0u64;
        loop {
            match worker.frame_rx.try_recv() {
                Ok(raw) => {
                    if latest.is_some() {
                        dropped += 1;
                    }
                    latest = Some(raw);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("V4L2 capture channel closed");
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
            pixel_format: FramePixelFormat::Rgb8,
            timestamp: raw.timestamp,
        }))
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
        }
        // Device was moved into the worker; reopen lazily next time it's needed.
        Ok(())
    }
}

impl Drop for V4l2Source {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
        }
    }
}
