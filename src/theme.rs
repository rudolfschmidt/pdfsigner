//! Visual constants and panel-styling helpers for the zathura/feh-style UI:
//! black bars with monospace text, white text in inputs, red accents in the
//! body, and no hover/active state changes on widgets.

use eframe::egui;

// Body accent — selection outline, body cursor, body selection bg.
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);

// Marquee selection rect.
pub const RUBBER_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 35, 60, 30);
pub const RUBBER_STROKE: egui::Color32 = egui::Color32::from_rgb(80, 140, 230);

// Footer status colours.
pub const ERROR_RED: egui::Color32 = egui::Color32::from_rgb(255, 60, 60);

// Header chrome.
pub const PLACEHOLDER_GRAY: egui::Color32 = egui::Color32::from_gray(140);
pub const SEPARATOR_DOT_GRAY: egui::Color32 = egui::Color32::from_gray(110);

pub const BAR_FONT_SIZE: f32 = 12.0;
pub const PANEL_MARGIN: egui::Margin = egui::Margin {
    left: 4.0,
    right: 4.0,
    top: 1.0,
    bottom: 1.0,
};

pub fn bar_font() -> egui::FontId {
    egui::FontId::monospace(BAR_FONT_SIZE)
}

pub fn black_panel_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(egui::Color32::BLACK)
        .inner_margin(PANEL_MARGIN)
}

/// Body-wide visuals applied at the start of every frame: red caret/selection
/// in the page area, no caret-preview-at-cursor.
pub fn apply_global(style: &mut egui::Style) {
    style.visuals.text_cursor.stroke.color = ACCENT;
    style.visuals.text_cursor.preview = false;
    style.visuals.selection.bg_fill = ACCENT;
}

/// Header inputs: white border, white selection bg, white cursor, flat —
/// no hover/active/focus state changes.
pub fn apply_header(style: &mut egui::Style) {
    style.visuals.extreme_bg_color = egui::Color32::BLACK;
    let white_border = egui::Stroke::new(1.0, egui::Color32::WHITE);
    style.visuals.widgets.inactive.bg_stroke = white_border;
    style.visuals.widgets.inactive.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.inactive.weak_bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.inactive.expansion = 0.0;
    let flat = style.visuals.widgets.inactive;
    style.visuals.widgets.hovered = flat;
    style.visuals.widgets.active = flat;
    style.visuals.selection.stroke = white_border;
    style.visuals.selection.bg_fill = egui::Color32::WHITE;
    style.visuals.text_cursor.stroke.color = egui::Color32::WHITE;
}

/// Custom-color-picker popup: white surfaces, black text/strokes, monospace 12,
/// no widget state shifts.
pub fn apply_popup(style: &mut egui::Style) {
    style.text_styles.insert(egui::TextStyle::Body, bar_font());
    style.text_styles.insert(egui::TextStyle::Button, bar_font());
    style.visuals.override_text_color = Some(egui::Color32::BLACK);
    let black_border = egui::Stroke::new(1.0, egui::Color32::BLACK);
    style.visuals.widgets.inactive.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.inactive.weak_bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.inactive.bg_stroke = black_border;
    style.visuals.widgets.inactive.fg_stroke = black_border;
    style.visuals.widgets.inactive.expansion = 0.0;
    let flat = style.visuals.widgets.inactive;
    style.visuals.widgets.hovered = flat;
    style.visuals.widgets.active = flat;
    style.visuals.extreme_bg_color = egui::Color32::WHITE;
    style.visuals.selection.stroke = black_border;
}
