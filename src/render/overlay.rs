use crate::capture::format::CaptureFormat;
use std::time::{Duration, Instant};

pub struct Toolbar {
    pub visible: bool,
    pub volume: f32,
    pub selected_format_index: usize,
    pub is_recording: bool,
    pub recording_start: Option<Instant>,
    last_mouse_over: Option<Instant>,
    auto_hide_delay: Duration,

    // Action flags — read and cleared by the app each frame
    pub screenshot_requested: bool,
    pub recording_toggled: bool,
    pub format_changed: bool,
}

impl Toolbar {
    pub fn new() -> Self {
        Self {
            visible: false,
            volume: 1.0,
            selected_format_index: 0,
            is_recording: false,
            recording_start: None,
            last_mouse_over: None,
            auto_hide_delay: Duration::from_secs(3),
            screenshot_requested: false,
            recording_toggled: false,
            format_changed: false,
        }
    }

    pub fn toggle_visible(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.last_mouse_over = Some(Instant::now());
        }
    }

    pub fn toggle_recording(&mut self) {
        self.is_recording = !self.is_recording;
        if self.is_recording {
            self.recording_start = Some(Instant::now());
        } else {
            self.recording_start = None;
        }
        self.recording_toggled = true;
    }

    pub fn recording_elapsed(&self) -> Duration {
        self.recording_start
            .map(|start| start.elapsed())
            .unwrap_or(Duration::ZERO)
    }

    pub fn ui(&mut self, ctx: &egui::Context, formats: &[CaptureFormat]) {
        // Auto-hide check
        if self.visible && !self.is_recording {
            if let Some(last) = self.last_mouse_over {
                if last.elapsed() > self.auto_hide_delay {
                    self.visible = false;
                }
            }
        }

        // Toggle pill button — always visible at bottom center
        egui::Area::new(egui::Id::new("toolbar_toggle"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let label = if self.visible {
                    "▼ Hide"
                } else {
                    "▲ Controls"
                };
                let button = egui::Button::new(label)
                    .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30))
                    .corner_radius(12.0);
                if ui.add(button).clicked() {
                    self.toggle_visible();
                }
            });

        // Toolbar panel — only when visible
        if self.visible {
            egui::TopBottomPanel::bottom("toolbar_panel")
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190))
                        .inner_margin(8.0),
                )
                .show(ctx, |ui| {
                    // Reset auto-hide when pointer is over toolbar
                    if ui.rect_contains_pointer(ui.max_rect()) {
                        self.last_mouse_over = Some(Instant::now());
                    }

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 16.0;

                        // Center the controls
                        ui.with_layout(
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                // Volume
                                ui.label("🔊");
                                let old_volume = self.volume;
                                ui.add(
                                    egui::Slider::new(&mut self.volume, 0.0..=1.0)
                                        .show_value(false),
                                );
                                if (self.volume - old_volume).abs() > f32::EPSILON {
                                    self.last_mouse_over = Some(Instant::now());
                                }

                                ui.separator();

                                // Resolution dropdown
                                let current_label = formats
                                    .get(self.selected_format_index)
                                    .map(|f| f.to_string())
                                    .unwrap_or_else(|| "—".to_string());
                                let old_index = self.selected_format_index;
                                egui::ComboBox::from_id_salt("format_selector")
                                    .selected_text(&current_label)
                                    .show_ui(ui, |ui| {
                                        for (i, fmt) in formats.iter().enumerate() {
                                            ui.selectable_value(
                                                &mut self.selected_format_index,
                                                i,
                                                fmt.to_string(),
                                            );
                                        }
                                    });
                                if self.selected_format_index != old_index {
                                    self.format_changed = true;
                                    self.last_mouse_over = Some(Instant::now());
                                }

                                ui.separator();

                                // Record button
                                if self.is_recording {
                                    let rec_button = egui::Button::new("⏹ Stop")
                                        .fill(egui::Color32::from_rgb(180, 30, 30));
                                    if ui.add(rec_button).clicked() {
                                        self.toggle_recording();
                                    }
                                } else {
                                    let rec_button = egui::Button::new("⏺ Rec")
                                        .fill(egui::Color32::from_rgb(80, 80, 80));
                                    if ui.add(rec_button).clicked() {
                                        self.toggle_recording();
                                    }
                                }

                                // Screenshot button
                                if ui.button("📸").clicked() {
                                    self.screenshot_requested = true;
                                }

                                // Recording timer
                                if self.is_recording {
                                    let elapsed = self.recording_elapsed();
                                    let total_secs = elapsed.as_secs();
                                    let h = total_secs / 3600;
                                    let m = (total_secs % 3600) / 60;
                                    let s = total_secs % 60;
                                    let timer_text = format!("{:02}:{:02}:{:02}", h, m, s);
                                    ui.label(
                                        egui::RichText::new(timer_text)
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                }
                            },
                        );
                    });
                });
        }

        // Request repaint for continuous timer updates
        ctx.request_repaint();
    }
}
