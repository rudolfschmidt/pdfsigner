//! Three feh-style popups that float on the foreground layer with a white
//! background and a 1px black border:
//!
//! * **Sig menu** — opened by right-press on the page; release on an entry
//!   inserts the signature image at the original press position.
//! * **Color menu** — toggled by `c`; left-click on a preset recolours all
//!   selected text overlays. The 4th entry "Custom…" opens the HSV picker.
//! * **Color custom** — full HSV picker hosted in an `egui::Area`.
//!
//! Each `render_*` function is a no-op when its menu state is `None`.

use eframe::egui;

use crate::theme;
use crate::App;

const ITEM_H: f32 = 18.0;
const ROW_PAD_X: f32 = 10.0;
const SWATCH_W: f32 = 18.0;
const SWATCH_H: f32 = 12.0;
const SWATCH_GAP: f32 = 8.0;

const COLOR_PRESETS: [(&str, Option<[u8; 3]>); 4] = [
    ("Black", Some([0, 0, 0])),
    ("Red", Some([200, 30, 30])),
    ("Blue", Some([30, 60, 200])),
    ("Custom…", None),
];

// ----------------------------------------------------------------------------
// Sig menu (press-hold-release on right mouse)
// ----------------------------------------------------------------------------

pub fn render_sig_menu(
    app: &mut App,
    ctx: &egui::Context,
    page_rect: egui::Rect,
    scale: f32,
    released: bool,
) {
    let Some(menu_pos) = app.sig_menu else { return };

    let labels: Vec<String> = app
        .signatures
        .iter()
        .map(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect();

    let font = theme::bar_font();
    let max_w = labels
        .iter()
        .map(|l| measure_w(ctx, l, font.clone()))
        .fold(80.0_f32, f32::max);
    let menu_w = max_w + ROW_PAD_X * 2.0;
    let menu_h = ITEM_H * labels.len() as f32;
    let menu_rect = egui::Rect::from_min_size(menu_pos, egui::vec2(menu_w, menu_h));

    let painter = popup_painter(ctx, "sig_menu");
    paint_popup_frame(&painter, menu_rect);

    let hover = ctx.input(|i| i.pointer.hover_pos());
    let mut hovered_idx: Option<usize> = None;
    for (i, label) in labels.iter().enumerate() {
        let row = row_rect(menu_pos, menu_w, i);
        let is_hover = hover.is_some_and(|h| row.contains(h));
        let (bg, fg) = row_colors(is_hover);
        if is_hover {
            hovered_idx = Some(i);
        }
        painter.rect_filled(row, 0.0, bg);
        painter.text(
            row.left_center() + egui::vec2(ROW_PAD_X, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            fg,
        );
    }

    if released {
        if let Some(idx) = hovered_idx
            && let Some(path) = app.signatures.get(idx).cloned() {
                // Insert at the original right-click position, not where the
                // user dragged to pick the menu entry.
                let pdf_x = (menu_pos.x - page_rect.min.x) / scale;
                let pdf_y = (menu_pos.y - page_rect.min.y) / scale;
                app.add_signature_at(path, pdf_x, pdf_y);
            }
        app.sig_menu = None;
    }
}

// ----------------------------------------------------------------------------
// Color menu (preset list + "Custom…" entry that opens the HSV picker)
// ----------------------------------------------------------------------------

pub fn render_color_menu(app: &mut App, ctx: &egui::Context) {
    let Some(menu_pos) = app.color_menu else { return };

    let font = theme::bar_font();
    let max_label_w = COLOR_PRESETS
        .iter()
        .map(|(l, _)| measure_w(ctx, l, font.clone()))
        .fold(0.0_f32, f32::max);
    let menu_w = ROW_PAD_X + SWATCH_W + SWATCH_GAP + max_label_w + ROW_PAD_X;
    let menu_h = ITEM_H * COLOR_PRESETS.len() as f32;
    let menu_rect = egui::Rect::from_min_size(menu_pos, egui::vec2(menu_w, menu_h));

    let painter = popup_painter(ctx, "color_menu");
    paint_popup_frame(&painter, menu_rect);

    let hover = ctx.input(|i| i.pointer.hover_pos());
    let mut hovered_idx: Option<usize> = None;
    for (i, (label, opt_rgb)) in COLOR_PRESETS.iter().enumerate() {
        let row = row_rect(menu_pos, menu_w, i);
        let is_hover = hover.is_some_and(|h| row.contains(h));
        let (bg, fg) = row_colors(is_hover);
        if is_hover {
            hovered_idx = Some(i);
        }
        painter.rect_filled(row, 0.0, bg);
        if let Some(rgb) = opt_rgb {
            let swatch_rect = egui::Rect::from_min_size(
                row.left_top() + egui::vec2(ROW_PAD_X, (ITEM_H - SWATCH_H) / 2.0),
                egui::vec2(SWATCH_W, SWATCH_H),
            );
            painter.rect_filled(swatch_rect, 0.0, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            painter.rect_stroke(swatch_rect, 0.0, egui::Stroke::new(1.0, fg));
        }
        painter.text(
            row.left_center() + egui::vec2(ROW_PAD_X + SWATCH_W + SWATCH_GAP, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            fg,
        );
    }

    let primary_pressed =
        ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    let esc_pressed = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if primary_pressed {
        if let Some(idx) = hovered_idx {
            match COLOR_PRESETS[idx].1 {
                Some(rgb) => app.apply_color_to_selected_text(rgb),
                None => app.color_custom = Some(menu_pos),
            }
        }
        app.color_menu = None;
        app.color_menu_consume_click = true;
    } else if esc_pressed {
        app.color_menu = None;
    }
}

// ----------------------------------------------------------------------------
// Custom HSV picker (egui::color_picker, hosted in a foreground Area)
// ----------------------------------------------------------------------------

pub fn render_color_custom(app: &mut App, ctx: &egui::Context) {
    let Some(custom_pos) = app.color_custom else { return };

    let init = app.first_selected_text_color().unwrap_or(app.text_color);
    let mut current = egui::Color32::from_rgb(init[0], init[1], init[2]);
    let mut changed = false;

    let area = egui::Area::new(egui::Id::new("color_custom_picker"))
        .order(egui::Order::Foreground)
        .fixed_pos(custom_pos)
        .show(ctx, |ui| {
            theme::apply_popup(ui.style_mut());
            egui::Frame::none()
                .fill(egui::Color32::WHITE)
                .stroke(egui::Stroke::new(1.0, egui::Color32::BLACK))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    changed = egui::color_picker::color_picker_color32(
                        ui,
                        &mut current,
                        egui::color_picker::Alpha::Opaque,
                    );
                });
        });

    if changed {
        app.apply_color_to_selected_text([current.r(), current.g(), current.b()]);
    }
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if esc || area.response.clicked_elsewhere() {
        app.color_custom = None;
        app.color_menu_consume_click = true;
    }
}

// ----------------------------------------------------------------------------
// Internal helpers
// ----------------------------------------------------------------------------

fn popup_painter(ctx: &egui::Context, id: &str) -> egui::Painter {
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new(id),
    ))
}

fn paint_popup_frame(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
}

fn row_rect(menu_pos: egui::Pos2, menu_w: f32, idx: usize) -> egui::Rect {
    egui::Rect::from_min_size(
        menu_pos + egui::vec2(0.0, idx as f32 * ITEM_H),
        egui::vec2(menu_w, ITEM_H),
    )
}

fn row_colors(hovered: bool) -> (egui::Color32, egui::Color32) {
    if hovered {
        (egui::Color32::BLACK, egui::Color32::WHITE)
    } else {
        (egui::Color32::WHITE, egui::Color32::BLACK)
    }
}

fn measure_w(ctx: &egui::Context, text: &str, font: egui::FontId) -> f32 {
    ctx.fonts(|f| {
        f.layout_no_wrap(text.to_string(), font, egui::Color32::WHITE)
            .size()
            .x
    })
}
