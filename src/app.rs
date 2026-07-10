//! The egui UI thread. Austere by design: a filename list fills the window and
//! a bottom transport carries the now-playing line, one progress bar, a
//! play/pause control, a volume slider, and the folder picker. The progress bar
//! is the only animated element (CLAUDE.md).

use std::sync::Arc;

use eframe::egui;

use crate::library;
use crate::player::Player;
use crate::track::Track;

/// Embedded so the app is self-contained regardless of installed fonts.
/// JetBrains Mono is licensed under the SIL OFL (see assets/JetBrainsMono-OFL.txt).
const JETBRAINS_MONO: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");

/// Install JetBrains Mono as the sole font and apply hyla's austere styling.
/// Call once with the egui context before the first frame.
pub fn configure(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "jetbrains_mono".to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO)),
    );
    // Use it for both families so the entire UI is monospaced.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "jetbrains_mono".to_owned());
    }
    ctx.set_fonts(fonts);

    // Restrained dark theme: near-black background, one muted accent for the
    // progress fill and selection. Minimal chrome.
    let mut visuals = egui::Visuals::dark();
    let accent = egui::Color32::from_rgb(0x44, 0x73, 0x25);
    visuals.panel_fill = egui::Color32::from_gray(14);
    visuals.extreme_bg_color = egui::Color32::from_gray(30);
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = egui::Stroke::NONE;
    visuals.override_text_color = Some(egui::Color32::from_gray(210));

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals = visuals.clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.slider_width = 120.0;
    });
}

pub struct HylaApp {
    player: Option<Player>,
    library: Vec<Track>,
    /// Index into `library` of the track currently playing, if any.
    current: Option<usize>,
    /// Local mirror of the player's volume, driven by the slider.
    volume: f32,
    /// Guards against issuing repeated auto-advances for a single track end.
    advance_issued: bool,
}

impl HylaApp {
    pub fn new(library: Vec<Track>) -> Self {
        Self {
            player: None,
            library,
            current: None,
            volume: 1.0,
            advance_issued: false,
        }
    }

    /// Start playing `library[index]`, reusing the open output stream if there
    /// is one, or creating the player on first play.
    fn play_index(&mut self, index: usize) {
        let Some(track) = self.library.get(index).cloned() else {
            return;
        };
        match &self.player {
            Some(player) => player.load(track.path.clone()),
            None => match Player::new(track.path.clone()) {
                Ok(player) => {
                    player.set_volume(self.volume);
                    self.player = Some(player);
                }
                Err(e) => {
                    eprintln!("failed to start playback: {e:#}");
                    return;
                }
            },
        }
        self.current = Some(index);
        self.advance_issued = false;
    }
}

impl eframe::App for HylaApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Solid near-black; the default is semi-transparent.
        egui::Color32::from_gray(14).to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Repaint continuously so the progress bar animates smoothly.
        ui.ctx().request_repaint();

        // Auto-advance to the next track when the current one finishes.
        let mut advance_to = None;
        if let (Some(player), Some(index)) = (self.player.as_ref(), self.current) {
            let finished = player.is_finished();
            if finished && !self.advance_issued && index + 1 < self.library.len() {
                advance_to = Some(index + 1);
            }
            self.advance_issued = finished;
        }
        if let Some(index) = advance_to {
            self.play_index(index);
        }

        // Transport pinned to the bottom, the track list filling the rest.
        // Both panels carry side margins so content clears the window borders.
        let margin = egui::Margin::symmetric(16, 12);
        let bottom_frame = egui::Frame::central_panel(ui.style()).inner_margin(margin);
        egui::Panel::bottom("transport")
            .frame(bottom_frame)
            .show(ui, |ui| self.transport(ui));

        let central_frame = egui::Frame::central_panel(ui.style()).inner_margin(margin);
        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| self.track_list(ui));
    }
}

impl HylaApp {
    /// Now-playing line, progress bar, times, and controls.
    fn transport(&mut self, ui: &mut egui::Ui) {
        let now_playing = self
            .current
            .and_then(|i| self.library.get(i))
            .map(|t| t.file_name.as_str())
            .unwrap_or("no track selected");
        ui.label(now_playing);
        ui.add_space(8.0);

        let (pos, dur) = self
            .player
            .as_ref()
            .map(|p| (p.position_secs(), p.duration_secs()))
            .unwrap_or((0.0, 0.0));
        let fraction = if dur > 0.0 { pos / dur } else { 0.0 };

        let bar = progress_bar(ui, fraction);
        if dur > 0.0
            && (bar.clicked() || bar.dragged())
            && let Some(pointer) = bar.interact_pointer_pos()
            && let Some(p) = &self.player
        {
            let t = ((pointer.x - bar.rect.left()) / bar.rect.width()).clamp(0.0, 1.0);
            p.seek((t * dur) as f64);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(fmt_time(pos));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(fmt_time(dur));
            });
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let paused = self.player.as_ref().map(|p| p.is_paused()).unwrap_or(true);
            let label = if paused { "play" } else { "pause" };
            if ui
                .add_enabled(self.player.is_some(), egui::Button::new(label))
                .clicked()
                && let Some(p) = &self.player
            {
                p.toggle_pause();
            }

            ui.add_space(12.0);
            ui.label("vol");
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false))
                .changed()
                && let Some(p) = &self.player
            {
                p.set_volume(self.volume);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("open folder").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.library = library::scan_dir(&dir);
                    self.current = None;
                }
            });
        });
    }

    /// Scrollable list of filenames; clicking one plays it.
    fn track_list(&mut self, ui: &mut egui::Ui) {
        if self.library.is_empty() {
            ui.label("no folder loaded — use “open folder”.");
            return;
        }
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, track) in self.library.iter().enumerate() {
                let selected = self.current == Some(index);
                if ui.selectable_label(selected, &track.file_name).clicked() {
                    clicked = Some(index);
                }
            }
        });
        if let Some(index) = clicked {
            self.play_index(index);
        }
    }
}

/// A minimal filled bar. Painted by hand rather than using a themed widget so
/// the look stays austere. Click or drag to seek.
fn progress_bar(ui: &mut egui::Ui, fraction: f32) -> egui::Response {
    let height = 8.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter();
    let rounding = egui::CornerRadius::same(2);
    let visuals = ui.visuals();
    painter.rect_filled(rect, rounding, visuals.extreme_bg_color);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    painter.rect_filled(fill, rounding, visuals.selection.bg_fill);
    response
}

fn fmt_time(secs: f32) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let total = secs as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}
