use anyhow::{Context, Result};
use chrono::Local;
use crossbeam_channel::Receiver;
use directories::UserDirs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// Generate the output path for a recording.
pub fn recording_path() -> PathBuf {
    let base = UserDirs::new()
        .and_then(|d| d.video_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = base.join("genki-arcade");
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    dir.join(format!("recording-{}.mp4", timestamp))
}

/// Configuration for the encoder.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
}

/// Trait for video+audio encoders that run on a dedicated thread.
pub trait Encoder: Send {
    fn start(
        &mut self,
        config: EncoderConfig,
        video_rx: Receiver<Vec<u8>>,
        audio_rx: Receiver<Vec<i16>>,
    ) -> Result<()>;
    fn stop(&mut self) -> Result<PathBuf>;
    fn is_running(&self) -> bool;
}

/// FFmpeg-based encoder producing H.264+AAC MP4 files.
pub struct FfmpegEncoder {
    handle: Option<JoinHandle<Result<()>>>,
    stop_flag: Arc<AtomicBool>,
    output_path: PathBuf,
}

impl FfmpegEncoder {
    pub fn new() -> Self {
        Self {
            handle: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            output_path: PathBuf::new(),
        }
    }
}

impl Encoder for FfmpegEncoder {
    fn start(
        &mut self,
        config: EncoderConfig,
        video_rx: Receiver<Vec<u8>>,
        audio_rx: Receiver<Vec<i16>>,
    ) -> Result<()> {
        let path = recording_path();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {:?}", parent))?;
        }

        self.output_path = path.clone();
        self.stop_flag.store(false, Ordering::SeqCst);
        let stop_flag = self.stop_flag.clone();

        let handle = thread::Builder::new()
            .name("encoder".into())
            .spawn(move || encode_loop(config, video_rx, audio_rx, stop_flag, path))
            .context("Failed to spawn encoder thread")?;

        self.handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self) -> Result<PathBuf> {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("Encoder thread panicked"))??;
        }
        Ok(self.output_path.clone())
    }

    fn is_running(&self) -> bool {
        self.handle.is_some()
    }
}

fn encode_loop(
    config: EncoderConfig,
    video_rx: Receiver<Vec<u8>>,
    audio_rx: Receiver<Vec<i16>>,
    stop_flag: Arc<AtomicBool>,
    path: PathBuf,
) -> Result<()> {
    use ffmpeg_next::channel_layout::ChannelLayout;
    use ffmpeg_next::codec::Compliance;
    use ffmpeg_next::format;
    use ffmpeg_next::software::scaling as sws;

    ffmpeg_next::init().context("Failed to initialize ffmpeg")?;

    let mut output_ctx =
        format::output(&path).with_context(|| format!("Failed to create output: {:?}", path))?;

    // Check global header flag before adding streams to avoid borrow conflicts
    let global_header = output_ctx
        .format()
        .flags()
        .contains(format::flag::Flags::GLOBAL_HEADER);

    // --- Video encoder setup ---
    let video_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264)
        .context("H264 encoder not found")?;

    let video_ctx = ffmpeg_next::codec::context::Context::new_with_codec(video_codec);
    let mut video_enc = video_ctx.encoder().video().context("Failed to create video encoder")?;

    video_enc.set_width(config.width);
    video_enc.set_height(config.height);
    video_enc.set_format(format::Pixel::YUV420P);
    video_enc.set_time_base((1, config.fps as i32));
    video_enc.set_frame_rate(Some((config.fps as i32, 1)));
    video_enc.set_bit_rate(8_000_000);
    video_enc.set_gop(config.fps * 2);
    video_enc.set_max_b_frames(2);

    if global_header {
        video_enc.set_flags(ffmpeg_next::codec::flag::Flags::GLOBAL_HEADER);
    }

    let mut video_enc = video_enc
        .open_as(video_codec)
        .context("Failed to open video encoder")?;

    {
        let mut video_stream = output_ctx
            .add_stream(video_codec)
            .context("Failed to add video stream")?;
        video_stream.set_parameters(&video_enc);
    }

    // --- Audio encoder setup ---
    let audio_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::AAC)
        .context("AAC encoder not found")?;

    let audio_ctx = ffmpeg_next::codec::context::Context::new_with_codec(audio_codec);
    let mut audio_enc = audio_ctx.encoder().audio().context("Failed to create audio encoder")?;

    audio_enc.set_rate(config.audio_sample_rate as i32);
    audio_enc.set_channel_layout(ChannelLayout::STEREO);
    audio_enc.set_format(format::Sample::F32(format::sample::Type::Planar));
    audio_enc.set_bit_rate(192_000);
    audio_enc.set_time_base((1, config.audio_sample_rate as i32));
    audio_enc.compliance(Compliance::Experimental);

    if global_header {
        audio_enc.set_flags(ffmpeg_next::codec::flag::Flags::GLOBAL_HEADER);
    }

    let mut audio_enc = audio_enc
        .open_as(audio_codec)
        .context("Failed to open audio encoder")?;

    let audio_frame_size = audio_enc.frame_size() as usize;

    {
        let mut audio_stream = output_ctx
            .add_stream(audio_codec)
            .context("Failed to add audio stream")?;
        audio_stream.set_parameters(&audio_enc);
    }

    // Stream indices: video=0, audio=1
    let video_stream_index: usize = 0;
    let audio_stream_index: usize = 1;

    // --- Write header ---
    output_ctx
        .write_header()
        .context("Failed to write output header")?;

    // Get time bases after header is written (muxer may adjust them)
    let video_time_base = output_ctx.stream(video_stream_index).unwrap().time_base();
    let audio_time_base = output_ctx.stream(audio_stream_index).unwrap().time_base();

    // --- Scaler for RGB24 -> YUV420P ---
    let mut scaler = sws::Context::get(
        format::Pixel::RGB24,
        config.width,
        config.height,
        format::Pixel::YUV420P,
        config.width,
        config.height,
        sws::Flags::BILINEAR,
    )
    .context("Failed to create scaler")?;

    let mut video_pts: i64 = 0;
    let mut audio_pts: i64 = 0;
    let mut audio_buffer: Vec<f32> = Vec::new();

    // Main encoding loop
    loop {
        if stop_flag.load(Ordering::SeqCst)
            && video_rx.is_empty()
            && audio_rx.is_empty()
        {
            break;
        }

        let mut did_work = false;

        // Process video frames
        if let Ok(rgb_data) = video_rx.try_recv() {
            did_work = true;
            let expected_size = (config.width * config.height * 3) as usize;
            if rgb_data.len() == expected_size {
                let mut src_frame =
                    ffmpeg_next::frame::Video::new(format::Pixel::RGB24, config.width, config.height);
                // Copy RGB data into source frame, row by row respecting stride
                let stride = src_frame.stride(0);
                let row_bytes = (config.width * 3) as usize;
                for y in 0..config.height as usize {
                    let src_offset = y * row_bytes;
                    let dst_offset = y * stride;
                    let dst = src_frame.data_mut(0);
                    dst[dst_offset..dst_offset + row_bytes]
                        .copy_from_slice(&rgb_data[src_offset..src_offset + row_bytes]);
                }

                let mut yuv_frame = ffmpeg_next::frame::Video::empty();
                scaler
                    .run(&src_frame, &mut yuv_frame)
                    .context("Scaler conversion failed")?;

                yuv_frame.set_pts(Some(video_pts));
                video_pts += 1;

                video_enc
                    .send_frame(&yuv_frame)
                    .context("Failed to send video frame")?;

                let mut packet = ffmpeg_next::Packet::empty();
                while video_enc.receive_packet(&mut packet).is_ok() {
                    packet.set_stream(video_stream_index);
                    packet.rescale_ts((1, config.fps as i32), video_time_base);
                    packet
                        .write_interleaved(&mut output_ctx)
                        .context("Failed to write video packet")?;
                }
            }
        }

        // Process audio samples
        if let Ok(samples) = audio_rx.try_recv() {
            did_work = true;
            // Convert i16 samples to f32 and accumulate
            for &s in &samples {
                audio_buffer.push(s as f32 / 32768.0);
            }

            let channels = config.audio_channels as usize;
            let samples_per_frame = if audio_frame_size > 0 {
                audio_frame_size
            } else {
                1024
            };
            let floats_per_frame = samples_per_frame * channels;

            while audio_buffer.len() >= floats_per_frame {
                let chunk: Vec<f32> = audio_buffer.drain(..floats_per_frame).collect();

                let mut frame = ffmpeg_next::frame::Audio::new(
                    format::Sample::F32(format::sample::Type::Planar),
                    samples_per_frame,
                    ChannelLayout::STEREO,
                );
                frame.set_rate(config.audio_sample_rate);
                frame.set_pts(Some(audio_pts));

                // Fill planar audio frame: deinterleave channels
                for ch in 0..channels {
                    let plane = frame.data_mut(ch);
                    for i in 0..samples_per_frame {
                        let val = chunk[i * channels + ch];
                        let bytes = val.to_ne_bytes();
                        let offset = i * 4; // f32 = 4 bytes
                        if offset + 4 <= plane.len() {
                            plane[offset..offset + 4].copy_from_slice(&bytes);
                        }
                    }
                }

                audio_pts += samples_per_frame as i64;

                audio_enc
                    .send_frame(&frame)
                    .context("Failed to send audio frame")?;

                let mut packet = ffmpeg_next::Packet::empty();
                while audio_enc.receive_packet(&mut packet).is_ok() {
                    packet.set_stream(audio_stream_index);
                    packet.rescale_ts(
                        (1, config.audio_sample_rate as i32),
                        audio_time_base,
                    );
                    packet
                        .write_interleaved(&mut output_ctx)
                        .context("Failed to write audio packet")?;
                }
            }
        }

        if !did_work {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    // Flush video encoder
    video_enc.send_eof().context("Failed to flush video encoder")?;
    let mut packet = ffmpeg_next::Packet::empty();
    while video_enc.receive_packet(&mut packet).is_ok() {
        packet.set_stream(video_stream_index);
        packet.rescale_ts((1, config.fps as i32), video_time_base);
        packet
            .write_interleaved(&mut output_ctx)
            .context("Failed to write flushed video packet")?;
    }

    // Flush audio encoder
    audio_enc.send_eof().context("Failed to flush audio encoder")?;
    let mut packet = ffmpeg_next::Packet::empty();
    while audio_enc.receive_packet(&mut packet).is_ok() {
        packet.set_stream(audio_stream_index);
        packet.rescale_ts(
            (1, config.audio_sample_rate as i32),
            audio_time_base,
        );
        packet
            .write_interleaved(&mut output_ctx)
            .context("Failed to write flushed audio packet")?;
    }

    output_ctx
        .write_trailer()
        .context("Failed to write output trailer")?;

    log::info!("Recording saved to {:?}", path);
    Ok(())
}
