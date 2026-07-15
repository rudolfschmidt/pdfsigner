//! Xournal++-style inline text editor for the overlay at `app.editing`.
//!
//! - **Enter** inserts a newline (egui multiline default).
//! - **Esc** / **Shift+Enter** finish the edit and deselect the overlay.
//! - Click-elsewhere finishes via `lost_focus()` (selection handled by that
//!   click's own handler in `draw_page`).
//! - Empty text on finish removes the overlay (no zombie label).

use eframe::egui;

use crate::overlay::{color_from_rgb, overlay_rect, selection_layouter, Overlay};
use crate::App;

pub fn render_inline_editor(
    app: &mut App,
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    page_rect: egui::Rect,
    scale: f32,
) {
    let Some(idx) = app.editing else { return };
    let cur = app.current;
    let just_focused = app.editing_just_focused;
    let cached_range = app.edit_cursor_range;

    let mut should_finish = false;
    let mut should_remove = false;
    let mut clear_selection = false;
    let mut new_cursor_range = None;

    if let Some(Overlay::Text {
        text,
        x,
        y,
        size_pt,
        color,
    }) = app.pages[cur].overlays.get_mut(idx)
    {
        let x_pt = *x;
        let y_pt = *y;
        let sz_pt = *size_pt;
        let font = egui::FontId::proportional(sz_pt * scale);
        let regular = color_from_rgb(*color);

        // edit_rect mirrors overlay_rect so the selection outline (drawn in
        // the overlay loop above us) wraps the TextEdit identically to view
        // mode — the text doesn't visually jump on enter/exit edit.
        let preview = Overlay::Text {
            text: text.clone(),
            x: x_pt,
            y: y_pt,
            size_pt: sz_pt,
            color: *color,
        };
        let edit_rect = overlay_rect(&preview, page_rect, scale, ctx);

        // Selected glyphs render white so they stay readable on the red
        // selection bg.
        let mut layouter =
            selection_layouter(cached_range, font.clone(), regular, egui::Color32::WHITE);

        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(edit_rect)
                .layout(*ui.layout()),
        );
        let output = egui::TextEdit::multiline(text)
            .font(font)
            .text_color(regular)
            .frame(false)
            .margin(egui::Vec2::ZERO)
            .desired_rows(1)
            .layouter(&mut layouter)
            .show(&mut child_ui);
        let resp = output.response;
        new_cursor_range = output.cursor_range;

        if !just_focused {
            resp.request_focus();
        }
        let (esc, shift_enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.modifiers.shift && i.key_pressed(egui::Key::Enter),
            )
        });
        if shift_enter && text.ends_with('\n') {
            // Shift+Enter is repurposed as a finish shortcut — strip the
            // newline egui's TextEdit just inserted.
            text.pop();
        }
        if esc || shift_enter {
            clear_selection = true;
            should_remove = text.is_empty();
            should_finish = true;
        } else if resp.lost_focus() {
            should_remove = text.is_empty();
            should_finish = true;
        }
    } else {
        // Index stale (e.g. overlay deleted) — clear edit state next.
        should_finish = true;
    }

    app.edit_cursor_range = new_cursor_range;
    if !just_focused {
        app.editing_just_focused = true;
    }
    if should_finish {
        app.editing = None;
        app.editing_just_focused = false;
        app.edit_cursor_range = None;
        if should_remove {
            app.pages[cur].overlays.remove(idx);
            app.selected.retain(|&i| i != idx);
            for s in app.selected.iter_mut() {
                if *s > idx {
                    *s -= 1;
                }
            }
        } else if clear_selection {
            app.selected.clear();
        }
    }
}
