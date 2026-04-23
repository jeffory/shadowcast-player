use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Sender;
use winit::application::ApplicationHandler;

use crate::stats::{FrameStats, StatsSnapshot, StatsTicker};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use shadowcast_core::{AppCommand as PluginCommand, AppEvent, Frame as PluginFrame};

use crate::capture::audio::{AudioSampleReceiver, AudioSource, CpalAudioSource};
use crate::capture::device;
use crate::capture::format::{CaptureFormat, FramePixelFormat};
use crate::capture::video::{PlatformVideoSource, VideoSource};
use crate::config::AppConfig;
use crate::plugin::PluginHost;
use crate::record::encoder::{Encoder, EncoderConfig, FfmpegEncoder};
use crate::record::screenshot::take_screenshot;
use crate::render::display::DisplayRenderer;
use crate::render::overlay::Toolbar;

/// Number of consecutive frame errors before considering the source disconnected.
const DISCONNECT_THRESHOLD: u32 = 30;

/// Number of consecutive redraws that `try_next_frame` returned `None` before
/// we treat the source as silently dead. At 60 fps request_redraw cadence this
/// is ~2 seconds — comfortably longer than any USB/AVFoundation hiccup, short
/// enough that the reconnect flow kicks in before the user gets annoyed.
const NO_FRAME_DISCONNECT_THRESHOLD: u32 = 120;

/// How often to attempt reconnection when the source is disconnected.
const RECONNECT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Retry interval for audio reconnection. Starts at 2s to catch USB audio
/// (UAC) devices that register a few seconds after the video (UVC) device,
/// then backs off so a missing-audio scenario doesn't stall the render thread
/// every 2 seconds forever. `CpalAudioSource::start` enumerates CoreAudio
/// devices and builds input+output streams, which can take 50-200ms — doing
/// that on a fixed 2s cadence is visible as a periodic frame drop.
fn audio_retry_interval(failure_count: u32) -> std::time::Duration {
    match failure_count {
        0..=1 => std::time::Duration::from_secs(2),
        2 => std::time::Duration::from_secs(4),
        3 => std::time::Duration::from_secs(8),
        _ => std::time::Duration::from_secs(30),
    }
}

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<DisplayRenderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    egui_ctx: egui::Context,
    video_source: Option<PlatformVideoSource>,
    audio_source: Option<CpalAudioSource>,
    encoder: Option<FfmpegEncoder>,
    video_frame_tx: Option<Sender<Arc<Vec<u8>>>>,
    toolbar: Toolbar,
    formats: Vec<CaptureFormat>,
    modifiers: ModifiersState,
    last_frame_data: Option<Arc<Vec<u8>>>,
    last_frame_width: u32,
    last_frame_height: u32,
    last_frame_format: FramePixelFormat,
    source_error_count: u32,
    no_frame_count: u32,
    source_connected: bool,
    audio_connected: bool,
    last_reconnect_attempt: Option<Instant>,
    last_audio_retry: Option<Instant>,
    audio_retry_failures: u32,
    /// Cached audio device name candidates for retry.
    audio_names: Vec<String>,
    #[allow(dead_code)]
    config: AppConfig,
    plugin_host: Option<PluginHost>,
    quit_requested: bool,
    stats: Arc<FrameStats>,
    stats_ticker: StatsTicker,
    last_stats_snapshot: Option<StatsSnapshot>,
    stats_enabled: bool,
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
            last_frame_data: None,
            last_frame_width: 0,
            last_frame_height: 0,
            last_frame_format: FramePixelFormat::Rgb8,
            source_error_count: 0,
            no_frame_count: 0,
            source_connected: false,
            audio_connected: false,
            last_reconnect_attempt: None,
            last_audio_retry: None,
            audio_retry_failures: 0,
            audio_names: Vec::new(),
            config: AppConfig::load(),
            plugin_host: None,
            quit_requested: false,
            stats: Arc::new(FrameStats::default()),
            stats_ticker: StatsTicker::new(),
            last_stats_snapshot: None,
            stats_enabled: false,
        }
    }

    fn start_recording(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded::<Arc<Vec<u8>>>(4);
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
                input_format: self.last_frame_format,
            };

            let mut encoder = FfmpegEncoder::new();
            if let Err(e) = encoder.start(config, rx, audio_rx) {
                log::error!("Failed to start encoder: {}", e);
                self.video_frame_tx = None;
                return;
            }
            self.encoder = Some(encoder);

            if let Some(host) = &self.plugin_host {
                let path = self.encoder.as_ref()
                    .map(|e| e.output_path().to_path_buf())
                    .unwrap_or_default();
                host.distribute_event(AppEvent::RecordingStarted { path });
            }

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
                Ok(path) => {
                    log::info!("Recording saved to {:?}", path);
                    if let Some(host) = &self.plugin_host {
                        host.distribute_event(AppEvent::RecordingStopped { path });
                    }
                }
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

        if let Some(host) = &self.plugin_host {
            if let Some(fmt) = self.formats.get(self.toolbar.selected_format_index).cloned() {
                host.distribute_event(AppEvent::FormatChanged { format: fmt });
            }
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
                self.audio_retry_failures = 0;
                true
            }
            Err(e) => {
                log::warn!("Audio not yet available: {:#}", e);
                false
            }
        }
    }

    /// Retries audio connection when video is connected but audio isn't.
    /// USB audio devices often register later than video, so we retry quickly
    /// at first, then back off — stream construction on CoreAudio is heavy
    /// enough to show up as a visible frame hitch if done every 2s.
    fn try_reconnect_audio(&mut self) {
        let interval = audio_retry_interval(self.audio_retry_failures);
        if let Some(last) = self.last_audio_retry {
            if last.elapsed() < interval {
                return;
            }
        }
        self.last_audio_retry = Some(Instant::now());
        let before = self.audio_retry_failures;
        if !self.try_start_audio() {
            self.audio_retry_failures = self.audio_retry_failures.saturating_add(1);
            if before < 4 && self.audio_retry_failures >= 4 {
                log::info!(
                    "Audio device not found after several attempts; retrying every 30s"
                );
            }
        }
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

        match PlatformVideoSource::new(video_path, self.stats.clone()) {
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
                if let Some(host) = &self.plugin_host {
                    host.distribute_event(AppEvent::DeviceConnected {
                        name: "ShadowCast".into(),
                    });
                }
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
        let video_path = capture_device.as_ref().map(|d| d.video_path.as_str());

        match video_path.map(|p| PlatformVideoSource::new(p, self.stats.clone())) {
            None => {
                log::info!("No capture device found, will retry on reconnect");
                self.source_connected = false;
            }
            Some(Err(e)) => {
                log::error!("Failed to open video device: {}", e);
                self.source_connected = false;
            }
            Some(Ok(mut source)) => {
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
        }

        // Initialize audio (may fail if USB audio isn't registered yet — will retry)
        self.audio_names = capture_device
            .as_ref()
            .map(|d| d.audio_matches.clone())
            .unwrap_or_else(|| vec!["ShadowCast".to_string()]);
        if !self.try_start_audio() {
            log::warn!("Audio not yet available, will retry in background");
        }

        // Initialize plugin host
        #[allow(unused_mut)]
        let mut plugin_host = PluginHost::new();

        #[cfg(feature = "example-logger")]
        {
            let logger_config = self
                .config
                .plugin_enabled("example-logger")
                .cloned()
                .unwrap_or_default();
            plugin_host.register(
                shadowcast_plugin_logger::LoggerPlugin,
                logger_config,
            );
        }

        self.plugin_host = Some(plugin_host);

        // Emit initial device connected event if source was found
        if self.source_connected {
            if let Some(host) = &self.plugin_host {
                host.distribute_event(AppEvent::DeviceConnected {
                    name: capture_device.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
                });
            }
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
                if let Some(mut host) = self.plugin_host.take() {
                    host.shutdown();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                if let Some(host) = &self.plugin_host {
                    host.distribute_event(AppEvent::WindowResized {
                        width: size.width,
                        height: size.height,
                    });
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
                    Key::Named(NamedKey::F12) => {
                        self.stats_enabled = !self.stats_enabled;
                        log::info!(
                            "Frame stats overlay {}",
                            if self.stats_enabled { "enabled" } else { "disabled" }
                        );
                        if !self.stats_enabled {
                            self.last_stats_snapshot = None;
                        }
                    }
                    Key::Character(c) if ctrl && c == "s" => {
                        if let Some(data) = &self.last_frame_data {
                            take_screenshot(
                                (**data).clone(),
                                self.last_frame_width,
                                self.last_frame_height,
                                self.last_frame_format,
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
                let frame_start = Instant::now();

                // 1. Get next video frame, or try to reconnect
                if !self.source_connected {
                    self.try_reconnect();
                } else if !self.audio_connected {
                    self.try_reconnect_audio();
                }

                if let Some(video) = &mut self.video_source {
                    match video.try_next_frame() {
                        Ok(Some(frame)) => {
                            self.source_error_count = 0;
                            self.no_frame_count = 0;
                            if !self.source_connected {
                                self.source_connected = true;
                                log::info!("Video source recovered");
                            }
                            self.last_frame_width = frame.width;
                            self.last_frame_height = frame.height;
                            self.last_frame_format = frame.pixel_format;

                            // Distribute frame to plugins
                            if let Some(host) = &self.plugin_host {
                                let plugin_frame = Arc::new(PluginFrame {
                                    width: frame.width,
                                    height: frame.height,
                                    data: frame.data.to_vec(),
                                    timestamp: frame.timestamp,
                                });
                                host.distribute_frame(plugin_frame);
                            }

                            // Send to encoder if recording
                            // Send to encoder if recording. `Arc::clone` is a
                            // refcount bump, not a buffer copy.
                            if let Some(tx) = &self.video_frame_tx {
                                let _ = tx.try_send(Arc::clone(&frame.data));
                            }

                            // Upload to renderer
                            if let Some(renderer) = &mut self.renderer {
                                renderer.upload_frame(
                                    &frame.data,
                                    frame.width,
                                    frame.height,
                                    frame.pixel_format,
                                );
                            }

                            self.last_frame_data = Some(frame.data);
                        }
                        Ok(None) => {
                            // No new frame since last redraw — re-present the
                            // previously uploaded texture so vsync cadence is
                            // preserved and a capture hitch doesn't show up
                            // as a peak-frame-time spike.
                            self.stats.inc_recv_stalled();
                            self.no_frame_count = self.no_frame_count.saturating_add(1);
                            if self.no_frame_count >= NO_FRAME_DISCONNECT_THRESHOLD
                                && self.source_connected
                            {
                                log::warn!(
                                    "Video source silent for {} redraws; treating as disconnected",
                                    self.no_frame_count
                                );
                                self.source_connected = false;
                                self.audio_connected = false;
                                self.video_source = None;
                                if let Some(renderer) = &mut self.renderer {
                                    renderer.clear_frame();
                                }
                                self.last_frame_data = None;
                                self.no_frame_count = 0;
                            }
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
                                if let Some(host) = &self.plugin_host {
                                    host.distribute_event(AppEvent::DeviceDisconnected);
                                }
                                // Drop the stale source and clear the display
                                self.video_source = None;
                                if let Some(renderer) = &mut self.renderer {
                                    renderer.clear_frame();
                                }
                                self.last_frame_data = None;
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
                    if let Some(data) = &self.last_frame_data {
                        take_screenshot(
                            (**data).clone(),
                            self.last_frame_width,
                            self.last_frame_height,
                            self.last_frame_format,
                        );
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

                // Poll plugin commands
                if let Some(host) = &self.plugin_host {
                    for cmd in host.poll_commands() {
                        match cmd {
                            PluginCommand::TakeScreenshot => {
                                self.toolbar.screenshot_requested = true;
                            }
                            PluginCommand::StartRecording => {
                                if !self.toolbar.is_recording {
                                    self.toolbar.toggle_recording();
                                }
                            }
                            PluginCommand::StopRecording => {
                                if self.toolbar.is_recording {
                                    self.toolbar.toggle_recording();
                                }
                            }
                            PluginCommand::SetFormat(fmt) => {
                                if let Some(idx) = self.formats.iter().position(|f| *f == fmt) {
                                    self.toolbar.selected_format_index = idx;
                                    self.toolbar.format_changed = true;
                                }
                            }
                            PluginCommand::ToggleFullscreen => {
                                if let Some(window) = &self.window {
                                    let fullscreen = if window.fullscreen().is_some() {
                                        None
                                    } else {
                                        Some(Fullscreen::Borderless(None))
                                    };
                                    window.set_fullscreen(fullscreen);
                                }
                            }
                            PluginCommand::Quit => {
                                log::info!("Quit requested by plugin");
                                self.quit_requested = true;
                            }
                        }
                    }
                }

                // Handle plugin quit request
                if self.quit_requested {
                    if self.toolbar.is_recording {
                        self.stop_recording();
                    }
                    if let Some(mut host) = self.plugin_host.take() {
                        host.shutdown();
                    }
                    event_loop.exit();
                    return;
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

                // Decide whether egui has anything to draw this frame.
                // When the source is connected and neither the toolbar nor the
                // stats overlay is visible, we can skip the entire egui
                // context run + tessellate + extra render pass, which is
                // worth ~1-3 ms per frame on a 60 Hz timeline.
                self.toolbar.tick();
                let needs_egui =
                    !self.source_connected || self.toolbar.visible || self.stats_enabled;

                if needs_egui {
                    let egui_state = self.egui_state.as_mut().unwrap();
                    let egui_renderer = self.egui_renderer.as_mut().unwrap();

                    let source_connected = self.source_connected;
                    let stats_enabled = self.stats_enabled;
                    let stats_snapshot = self.last_stats_snapshot;
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
                                            egui::RichText::new(
                                                "Connect a capture device to begin",
                                            )
                                            .size(14.0)
                                            .color(egui::Color32::from_gray(100)),
                                        );
                                        ui.add_space(4.0);
                                    });
                                });
                        }
                        self.toolbar.ui(ctx, &self.formats);
                        if stats_enabled {
                            draw_stats_overlay(ctx, stats_snapshot);
                        }
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
                }

                renderer.queue.submit(std::iter::once(encoder.finish()));
                output.present();

                // 5. Stats bookkeeping
                self.stats.inc_rendered();
                self.stats
                    .record_frame_us(frame_start.elapsed().as_micros() as u64);
                // Always tick so the counter deltas stay aligned, but only
                // surface (log / display) when the overlay is enabled.
                if let Some(snap) = self.stats_ticker.tick(&self.stats) {
                    if self.stats_enabled {
                        log::info!("frame stats: {}", snap.summary());
                        self.last_stats_snapshot = Some(snap);
                    }
                }

                // 6. Request next redraw
                window.request_redraw();
            }

            _ => {}
        }
    }
}

/// Draw the frame-stats overlay at the top-left corner. Toggled with F12.
fn draw_stats_overlay(ctx: &egui::Context, snapshot: Option<StatsSnapshot>) {
    egui::Area::new(egui::Id::new("frame_stats_overlay"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let label = |text: String| {
                        egui::RichText::new(text)
                            .monospace()
                            .size(12.0)
                            .color(egui::Color32::from_gray(220))
                    };
                    match snapshot {
                        None => {
                            ui.label(label("frame stats: collecting…".to_string()));
                        }
                        Some(s) => {
                            let peak_ms = s.peak_frame_us as f64 / 1000.0;
                            let peak_color = if peak_ms > 25.0 {
                                egui::Color32::from_rgb(255, 120, 120)
                            } else if peak_ms > 18.0 {
                                egui::Color32::from_rgb(240, 200, 120)
                            } else {
                                egui::Color32::from_gray(220)
                            };
                            let drop_color = if s.dropped_per_sec > 0 {
                                egui::Color32::from_rgb(255, 120, 120)
                            } else {
                                egui::Color32::from_gray(220)
                            };
                            ui.label(label(format!(
                                "captured {}/s   rendered {}/s",
                                s.captured_per_sec, s.rendered_per_sec
                            )));
                            ui.label(
                                egui::RichText::new(format!(
                                    "dropped at capture {}/s",
                                    s.dropped_per_sec
                                ))
                                .monospace()
                                .size(12.0)
                                .color(drop_color),
                            );
                            ui.label(label(format!(
                                "dropped at render {}/s",
                                s.dropped_at_render_per_sec
                            )));
                            ui.label(
                                egui::RichText::new(format!("peak frame {:.2} ms", peak_ms))
                                    .monospace()
                                    .size(12.0)
                                    .color(peak_color),
                            );
                        }
                    }
                });
        });
}
