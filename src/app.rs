use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Sender;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::capture::audio::{AudioSampleReceiver, AudioSource, CpalAudioSource};
use crate::capture::device;
use crate::capture::format::CaptureFormat;
use crate::capture::video::{PlatformVideoSource, VideoSource};
use crate::record::encoder::{Encoder, EncoderConfig, FfmpegEncoder};
use crate::record::screenshot::take_screenshot;
use crate::render::display::DisplayRenderer;
use crate::render::overlay::Toolbar;

/// Number of consecutive frame errors before considering the source disconnected.
const DISCONNECT_THRESHOLD: u32 = 30;

/// How often to attempt reconnection when the source is disconnected.
const RECONNECT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// How often to retry audio when video is connected but audio isn't.
/// USB audio devices (UAC) often take longer to register than video (UVC).
const AUDIO_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<DisplayRenderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    egui_ctx: egui::Context,
    video_source: Option<PlatformVideoSource>,
    audio_source: Option<CpalAudioSource>,
    encoder: Option<FfmpegEncoder>,
    video_frame_tx: Option<Sender<Vec<u8>>>,
    toolbar: Toolbar,
    formats: Vec<CaptureFormat>,
    modifiers: ModifiersState,
    last_frame_rgb: Option<Vec<u8>>,
    last_frame_width: u32,
    last_frame_height: u32,
    source_error_count: u32,
    source_connected: bool,
    audio_connected: bool,
    last_reconnect_attempt: Option<Instant>,
    last_audio_retry: Option<Instant>,
    /// Cached audio device name candidates for retry.
    audio_names: Vec<String>,
}

impl Drop for App {
    fn drop(&mut self) {
        // Drop GPU and egui resources before the window so that wgpu's
        // EGL teardown and smithay_clipboard's Wayland cleanup can use
        // the still-live Wayland connection.
        self.egui_renderer = None;
        self.egui_state = None;
        self.renderer = None;
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            egui_state: None,
            egui_renderer: None,
            egui_ctx: egui::Context::default(),
            video_source: None,
            audio_source: None,
            encoder: None,
            video_frame_tx: None,
            toolbar: Toolbar::new(),
            formats: Vec::new(),
            modifiers: ModifiersState::empty(),
            last_frame_rgb: None,
            last_frame_width: 0,
            last_frame_height: 0,
            source_error_count: 0,
            source_connected: false,
            audio_connected: false,
            last_reconnect_attempt: None,
            last_audio_retry: None,
            audio_names: Vec::new(),
        }
    }

    fn start_recording(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded(4);
        self.video_frame_tx = Some(tx);

        let audio_rx = self
            .audio_source
            .as_ref()
            .and_then(|a| a.audio_receiver())
            .unwrap_or_else(|| {
                let (_tx, rx) = crossbeam_channel::bounded(1);
                rx
            });

        let format = self
            .formats
            .get(self.toolbar.selected_format_index)
            .cloned();

        if let Some(fmt) = format {
            let config = EncoderConfig {
                width: fmt.width,
                height: fmt.height,
                fps: fmt.fps,
                audio_sample_rate: 48000,
                audio_channels: 2,
            };

            let mut encoder = FfmpegEncoder::new();
            if let Err(e) = encoder.start(config, rx, audio_rx) {
                log::error!("Failed to start encoder: {}", e);
                self.video_frame_tx = None;
                return;
            }
            self.encoder = Some(encoder);

            if let Some(audio) = &self.audio_source {
                audio.set_recording(true);
            }

            log::info!("Recording started");
        }
    }

    fn stop_recording(&mut self) {
        if let Some(audio) = &self.audio_source {
            audio.set_recording(false);
        }

        // Drop the sender to signal EOF to encoder
        self.video_frame_tx = None;

        if let Some(mut encoder) = self.encoder.take() {
            match encoder.stop() {
                Ok(path) => log::info!("Recording saved to {:?}", path),
                Err(e) => log::error!("Failed to stop encoder: {}", e),
            }
        }

        log::info!("Recording stopped");
    }

    fn handle_format_change(&mut self) {
        let Some(video) = &mut self.video_source else {
            return;
        };
        let Some(format) = self
            .formats
            .get(self.toolbar.selected_format_index)
            .cloned()
        else {
            return;
        };

        if let Err(e) = video.stop() {
            log::error!("Failed to stop video stream: {}", e);
        }
        if let Err(e) = video.set_format(&format) {
            log::error!("Failed to set format: {}", e);
        }
        if let Err(e) = video.start() {
            log::error!("Failed to restart video stream: {}", e);
        }

        // Resize window to new resolution
        if let Some(window) = &self.window {
            let _ = window.request_inner_size(LogicalSize::new(format.width, format.height));
        }
    }

    /// Attempts to start audio capture using the cached device name candidates.
    /// Returns true if audio started successfully.
    fn try_start_audio(&mut self) -> bool {
        let names: Vec<&str> = self.audio_names.iter().map(|s| s.as_str()).collect();
        let mut audio = CpalAudioSource::new(&names);
        match audio.start() {
            Ok(()) => {
                log::info!("Audio connected");
                self.audio_source = Some(audio);
                self.audio_connected = true;
                true
            }
            Err(e) => {
                log::warn!("Audio not yet available: {:#}", e);
                false
            }
        }
    }

    /// Retries audio connection when video is connected but audio isn't.
    /// USB audio devices often register later than video on Linux.
    fn try_reconnect_audio(&mut self) {
        if let Some(last) = self.last_audio_retry {
            if last.elapsed() < AUDIO_RETRY_INTERVAL {
                return;
            }
        }
        self.last_audio_retry = Some(Instant::now());
        self.try_start_audio();
    }

    /// Attempts to rediscover and reopen the video (and audio) capture device.
    /// Called periodically when the source is disconnected.
    fn try_reconnect(&mut self) {
        // Throttle reconnection attempts
        if let Some(last) = self.last_reconnect_attempt {
            if last.elapsed() < RECONNECT_INTERVAL {
                return;
            }
        }
        self.last_reconnect_attempt = Some(Instant::now());

        let capture_device = device::find_shadowcast();
        let video_path = match capture_device.as_ref() {
            Some(d) => d.video_path.as_str(),
            None => return, // No device found, try again later
        };

        log::info!(
            "Attempting to reconnect to capture device at {}",
            video_path
        );

        match PlatformVideoSource::new(video_path) {
            Ok(mut source) => {
                self.formats = source.supported_formats();

                let default_index = self
                    .formats
                    .iter()
                    .position(|f| {
                        f.pixel_format == crate::capture::format::PixelFormat::Mjpeg
                            && f.width == 1920
                            && f.height == 1080
                            && f.fps == 60
                    })
                    .unwrap_or(0);

                self.toolbar.selected_format_index = default_index;

                if let Some(fmt) = self.formats.get(default_index) {
                    if let Err(e) = source.set_format(fmt) {
                        log::error!("Failed to set video format on reconnect: {}", e);
                        return;
                    }
                }

                if let Err(e) = source.start() {
                    log::error!("Failed to start video stream on reconnect: {}", e);
                    return;
                }

                self.video_source = Some(source);
                self.source_error_count = 0;
                self.source_connected = true;
                log::info!("Video source reconnected");

                // Reconnect audio too
                self.audio_names = capture_device
                    .as_ref()
                    .map(|d| d.audio_matches.clone())
                    .unwrap_or_else(|| vec!["ShadowCast".to_string()]);
                if !self.try_start_audio() {
                    log::debug!("Audio not yet available on reconnect, will retry");
                }
            }
            Err(_) => {
                // Device found but couldn't open — will retry next interval
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Create window
        let attrs = Window::default_attributes()
            .with_title("shadowcast-player")
            .with_inner_size(LogicalSize::new(1920u32, 1080u32))
            .with_visible(false);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("Failed to create window: {}", e);
                event_loop.exit();
                return;
            }
        };

        // Create DisplayRenderer
        let renderer = match pollster::block_on(DisplayRenderer::new(window.clone())) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Failed to create renderer: {}", e);
                event_loop.exit();
                return;
            }
        };

        // Create egui state and renderer
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
            None,
        );

        let egui_renderer =
            egui_wgpu::Renderer::new(&renderer.device, renderer.surface_format(), None, 1, false);

        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);

        // Discover the ShadowCast device
        let capture_device = device::find_shadowcast();

        // Initialize video capture
        let video_path = capture_device
            .as_ref()
            .map(|d| d.video_path.as_str())
            .unwrap_or("/dev/video2");

        match PlatformVideoSource::new(video_path) {
            Ok(mut source) => {
                self.formats = source.supported_formats();

                // Find MJPEG 1080p60 or fall back to first format
                let default_index = self
                    .formats
                    .iter()
                    .position(|f| {
                        f.pixel_format == crate::capture::format::PixelFormat::Mjpeg
                            && f.width == 1920
                            && f.height == 1080
                            && f.fps == 60
                    })
                    .unwrap_or(0);

                self.toolbar.selected_format_index = default_index;

                if let Some(fmt) = self.formats.get(default_index) {
                    if let Err(e) = source.set_format(fmt) {
                        log::error!("Failed to set video format: {}", e);
                    }
                }

                if let Err(e) = source.start() {
                    log::error!("Failed to start video stream: {}", e);
                }

                self.video_source = Some(source);
                self.source_connected = true;
            }
            Err(e) => {
                log::error!("Failed to open video device: {}", e);
                self.source_connected = false;
            }
        }

        // Initialize audio (may fail if USB audio isn't registered yet — will retry)
        self.audio_names = capture_device
            .as_ref()
            .map(|d| d.audio_matches.clone())
            .unwrap_or_else(|| vec!["ShadowCast".to_string()]);
        if !self.try_start_audio() {
            log::warn!("Audio not yet available, will retry in background");
        }

        // Store renderer and window, make visible
        self.renderer = Some(renderer);
        window.set_visible(true);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Pass events to egui first
        if let Some(egui_state) = &mut self.egui_state {
            if let Some(window) = &self.window {
                let response = egui_state.on_window_event(window, &event);
                if response.consumed {
                    return;
                }
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                if self.toolbar.is_recording {
                    self.stop_recording();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }

            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let ctrl = self.modifiers.control_key();
                match logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        if self.toolbar.is_recording {
                            self.stop_recording();
                        }
                        event_loop.exit();
                    }
                    Key::Named(NamedKey::F11) => {
                        if let Some(window) = &self.window {
                            let fullscreen = if window.fullscreen().is_some() {
                                None
                            } else {
                                Some(Fullscreen::Borderless(None))
                            };
                            window.set_fullscreen(fullscreen);
                        }
                    }
                    Key::Character(c) if ctrl && c == "s" => {
                        if let Some(rgb) = &self.last_frame_rgb {
                            take_screenshot(
                                rgb.clone(),
                                self.last_frame_width,
                                self.last_frame_height,
                            );
                        }
                    }
                    Key::Character(c) if ctrl && c == "r" => {
                        self.toolbar.toggle_recording();
                        // recording_toggled flag is handled in RedrawRequested
                    }
                    _ => {}
                }
            }

            WindowEvent::CursorMoved { .. } => {
                self.toolbar.on_mouse_move();
                if let Some(window) = &self.window {
                    window.set_cursor_visible(true);
                }
            }

            WindowEvent::RedrawRequested => {
                // 1. Get next video frame, or try to reconnect
                if !self.source_connected {
                    self.try_reconnect();
                } else if !self.audio_connected {
                    self.try_reconnect_audio();
                }

                if let Some(video) = &mut self.video_source {
                    match video.next_frame() {
                        Ok(frame) => {
                            self.source_error_count = 0;
                            if !self.source_connected {
                                self.source_connected = true;
                                log::info!("Video source recovered");
                            }
                            self.last_frame_width = frame.width;
                            self.last_frame_height = frame.height;

                            // Send to encoder if recording
                            if let Some(tx) = &self.video_frame_tx {
                                let _ = tx.try_send(frame.data.clone());
                            }

                            // Upload to renderer
                            if let Some(renderer) = &mut self.renderer {
                                renderer.upload_frame(&frame.data, frame.width, frame.height);
                            }

                            self.last_frame_rgb = Some(frame.data);
                        }
                        Err(e) => {
                            self.source_error_count += 1;
                            if self.source_error_count >= DISCONNECT_THRESHOLD
                                && self.source_connected
                            {
                                log::warn!(
                                    "Video source disconnected after {} errors",
                                    self.source_error_count
                                );
                                self.source_connected = false;
                                self.audio_connected = false;
                                // Drop the stale source and clear the display
                                self.video_source = None;
                                if let Some(renderer) = &mut self.renderer {
                                    renderer.clear_frame();
                                }
                                self.last_frame_rgb = None;
                            } else if self.source_error_count == 1 {
                                log::warn!("Frame capture error: {}", e);
                            } else {
                                log::debug!(
                                    "Frame capture error ({}x): {}",
                                    self.source_error_count,
                                    e
                                );
                            }
                        }
                    }
                }

                // 2. Check toolbar action flags
                if self.toolbar.format_changed {
                    self.toolbar.format_changed = false;
                    self.handle_format_change();
                }

                if self.toolbar.screenshot_requested {
                    self.toolbar.screenshot_requested = false;
                    if let Some(rgb) = &self.last_frame_rgb {
                        take_screenshot(rgb.clone(), self.last_frame_width, self.last_frame_height);
                    }
                }

                if self.toolbar.recording_toggled {
                    self.toolbar.recording_toggled = false;
                    if self.toolbar.is_recording {
                        self.start_recording();
                    } else {
                        self.stop_recording();
                    }
                }

                if self.toolbar.scale_mode_changed {
                    self.toolbar.scale_mode_changed = false;
                }

                // Update scale mode every frame (cheap buffer write)
                if let Some(renderer) = &self.renderer {
                    renderer.set_scale_mode(
                        self.toolbar.scale_mode,
                        self.last_frame_width,
                        self.last_frame_height,
                    );
                }

                // Hide cursor when toolbar is hidden (VLC-style)
                if !self.toolbar.visible {
                    if let Some(window) = &self.window {
                        window.set_cursor_visible(false);
                    }
                }

                // 3. Update audio volume
                if let Some(audio) = &self.audio_source {
                    audio.set_volume(self.toolbar.volume);
                }

                // 4. Render
                let Some(renderer) = &self.renderer else {
                    return;
                };
                let Some(window) = &self.window else {
                    return;
                };

                let (output, mut encoder) = match renderer.render_frame() {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("Render error: {}", e);
                        return;
                    }
                };

                // Run egui
                let egui_state = self.egui_state.as_mut().unwrap();
                let egui_renderer = self.egui_renderer.as_mut().unwrap();

                let source_connected = self.source_connected;
                let raw_input = egui_state.take_egui_input(window);
                let full_output = self.egui_ctx.run(raw_input, |ctx| {
                    if !source_connected {
                        egui::Area::new(egui::Id::new("disconnected_overlay"))
                            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                            .order(egui::Order::Background)
                            .show(ctx, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new("Source disconnected")
                                            .size(24.0)
                                            .color(egui::Color32::from_gray(160)),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new("Connect a capture device to begin")
                                            .size(14.0)
                                            .color(egui::Color32::from_gray(100)),
                                    );
                                    ui.add_space(4.0);
                                });
                            });
                    }
                    self.toolbar.ui(ctx, &self.formats);
                });
                egui_state.handle_platform_output(window, full_output.platform_output);

                let clipped = self
                    .egui_ctx
                    .tessellate(full_output.shapes, full_output.pixels_per_point);
                let screen_desc = egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [
                        renderer.surface_config.width,
                        renderer.surface_config.height,
                    ],
                    pixels_per_point: full_output.pixels_per_point,
                };

                // Update egui textures
                for (id, delta) in &full_output.textures_delta.set {
                    egui_renderer.update_texture(&renderer.device, &renderer.queue, *id, delta);
                }

                egui_renderer.update_buffers(
                    &renderer.device,
                    &renderer.queue,
                    &mut encoder,
                    &clipped,
                    &screen_desc,
                );

                // Render egui on top of video
                let view = output.texture.create_view(&Default::default());
                {
                    let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    let mut render_pass = render_pass.forget_lifetime();
                    egui_renderer.render(&mut render_pass, &clipped, &screen_desc);
                }

                // Free egui textures
                for id in &full_output.textures_delta.free {
                    egui_renderer.free_texture(id);
                }

                renderer.queue.submit(std::iter::once(encoder.finish()));
                output.present();

                // 5. Request next redraw
                window.request_redraw();
            }

            _ => {}
        }
    }
}
