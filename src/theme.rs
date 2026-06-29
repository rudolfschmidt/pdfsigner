//! Visual constants and panel-styling helpers for the zathura/feh-style UI:
//! black bars with monospace text, white text in inputs, red accents in the
//! body, and no hover/active state changes on widgets.

use eframe::egui;

/// Top-of-text → baseline distance as a fraction of font size, for Inter
/// laid out by egui via `FontId::proportional`. egui places a galley's
/// baseline `font_ascent` points below the top edge, and `painter.text` with
/// `Align2::LEFT_TOP` pins that top edge to the draw position; the PDF writer
/// reuses this ratio so saved text sits exactly where the preview shows it.
/// Measured empirically — see the `inter_baseline_ratio` test below.
pub const INTER_BASELINE_RATIO: f32 = 0.969;

/// Line-to-line advance as a fraction of font size, for Inter laid out by
/// egui. Multi-line overlay text wraps at this pitch in the preview, so the
/// PDF writer advances each line by the same amount. Measured empirically.
pub const INTER_LINE_HEIGHT_RATIO: f32 = 1.21;

/// Register the bundled Inter font as egui's proportional family so the
/// on-screen overlay text matches the glyphs the PDF writer embeds. Bars stay
/// on the default monospace font (they use `FontId::monospace`).
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("Inter".to_owned(), egui::FontData::from_static(crate::pdf::INTER_TTF));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".to_owned());
    ctx.set_fonts(fonts);
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `INTER_BASELINE_RATIO` against egui's actual layout: lay out a
    /// large Inter galley and read the baseline (`glyph.pos.y`) of the first
    /// row. Measuring at a big size keeps egui's pixel-rounding negligible.
    #[test]
    fn inter_baseline_ratio() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        // Run one empty frame so `set_fonts` takes effect.
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        const SIZE: f32 = 1000.0;
        let galley = ctx.fonts(|f| {
            f.layout_no_wrap("Hg".to_owned(), egui::FontId::proportional(SIZE), egui::Color32::BLACK)
        });
        let baseline = galley.rows[0].glyphs[0].pos.y;
        let ratio = baseline / SIZE;
        let line_ratio = ctx.fonts(|f| f.row_height(&egui::FontId::proportional(SIZE))) / SIZE;
        println!("measured INTER_BASELINE_RATIO   = {ratio:.6}");
        println!("measured INTER_LINE_HEIGHT_RATIO = {line_ratio:.6}");
        assert!(
            (ratio - INTER_BASELINE_RATIO).abs() < 0.002,
            "INTER_BASELINE_RATIO is {INTER_BASELINE_RATIO} but egui lays Inter out at {ratio}"
        );
        assert!(
            (line_ratio - INTER_LINE_HEIGHT_RATIO).abs() < 0.01,
            "INTER_LINE_HEIGHT_RATIO is {INTER_LINE_HEIGHT_RATIO} but egui uses {line_ratio}"
        );
    }
}
