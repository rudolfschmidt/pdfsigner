//! The `Overlay` model (text or image placed on a PDF page) and shared
//! geometry / cursor helpers used by both the rendering and the editor.

use eframe::egui;
use std::path::PathBuf;

#[derive(Clone)]
pub enum Overlay {
    Image { path: PathBuf, x: f32, y: f32, w: f32, h: f32 },
    Text { text: String, x: f32, y: f32, size_pt: f32, color: [u8; 3] },
    /// Opaque black rectangle covering the area (visual redaction).
    /// Note: the underlying text objects in the source PDF are NOT removed —
    /// `pdftotext` can still extract them. Run `pdf.sanitize` afterwards for
    /// permanent redaction.
    Redact { x: f32, y: f32, w: f32, h: f32 },
    /// Rose-tinted translucent highlight awaiting commit — the user picked
    /// this area in mark mode but hasn't turned it into a redaction yet.
    /// NOT persisted in the saved PDF (transient UI state).
    PendingMark { x: f32, y: f32, w: f32, h: f32 },
}

impl Overlay {
    /// Top-left position in PDF points, mutable.
    pub fn position_mut(&mut self) -> (&mut f32, &mut f32) {
        match self {
            Overlay::Image { x, y, .. }
            | Overlay::Text { x, y, .. }
            | Overlay::Redact { x, y, .. }
            | Overlay::PendingMark { x, y, .. } => (x, y),
        }
    }

    pub fn is_redact(&self) -> bool {
        matches!(self, Overlay::Redact { .. })
    }

    pub fn is_pending_mark(&self) -> bool {
        matches!(self, Overlay::PendingMark { .. })
    }
}

/// Screen-space rect for an overlay given the page rect and scale (pt → px).
///
/// Text rects use egui's natural galley metrics so the rect matches what the
/// painter draws and what the inline `TextEdit` lays out. Empty text falls
/// back to a small visible minimum so a freshly-spawned, in-edit overlay still
/// has a clickable / drawable cursor area.
pub fn overlay_rect(o: &Overlay, page_rect: egui::Rect, scale: f32, ctx: &egui::Context) -> egui::Rect {
    match o {
        Overlay::Image { x, y, w, h, .. }
        | Overlay::Redact { x, y, w, h }
        | Overlay::PendingMark { x, y, w, h } => egui::Rect::from_min_size(
            page_rect.min + egui::vec2(x * scale, y * scale),
            egui::vec2(w * scale, h * scale),
        ),
        Overlay::Text { text, x, y, size_pt, .. } => {
            let fid = egui::FontId::proportional(size_pt * scale);
            let size = ctx.fonts(|f| {
                f.layout_no_wrap(text.clone(), fid.clone(), egui::Color32::BLACK).size()
            });
            let (w, h) = if text.is_empty() {
                let row_h = ctx.fonts(|f| f.row_height(&fid));
                (20.0, row_h)
            } else {
                (size.x, size.y)
            };
            egui::Rect::from_min_size(
                page_rect.min + egui::vec2(x * scale, y * scale),
                egui::vec2(w, h),
            )
        }
    }
}

/// Topmost overlay containing `pos`; iterates back-to-front so latest-drawn wins.
pub fn hit_test(rects: &[egui::Rect], pos: egui::Pos2) -> Option<usize> {
    rects
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, r)| r.contains(pos).then_some(i))
}

/// Screen pixel → PDF point (top-down), given the page rect and pt-to-px scale.
pub fn screen_to_pdf(pos: egui::Pos2, page_rect: egui::Rect, scale: f32) -> (f32, f32) {
    ((pos.x - page_rect.min.x) / scale, (pos.y - page_rect.min.y) / scale)
}

/// Build an opaque `Color32` from a stored `[R, G, B]` triple.
pub fn color_from_rgb(rgb: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// Char-index span of a `CursorRange`, ordered (low, high). `None` for empty.
pub fn selection_span(range: egui::text_selection::CursorRange) -> Option<(usize, usize)> {
    let p = range.primary.ccursor.index;
    let s = range.secondary.ccursor.index;
    let (lo, hi) = if p < s { (p, s) } else { (s, p) };
    (lo != hi).then_some((lo, hi))
}

/// Convert a character index to a byte index in a UTF-8 string. Out-of-range
/// returns `s.len()` (one past the end).
pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// `TextEdit::layouter` callback that paints the selected character range in
/// `selected_color` and everything else in `regular_color`. The selection comes
/// from the previous frame's `cursor_range` (egui's layouter runs before the
/// widget knows the current frame's selection — the one-frame lag is invisible).
pub fn selection_layouter(
    cached: Option<egui::text_selection::CursorRange>,
    font: egui::FontId,
    regular: egui::Color32,
    selected: egui::Color32,
) -> impl FnMut(&egui::Ui, &str, f32) -> std::sync::Arc<egui::Galley> {
    move |ui: &egui::Ui, text: &str, _wrap_width: f32| {
        let mut job = egui::text::LayoutJob::default();
        let fmt = |c: egui::Color32| egui::TextFormat {
            font_id: font.clone(),
            color: c,
            ..Default::default()
        };
        if let Some((lo, hi)) = cached.and_then(selection_span) {
            let lo_b = char_to_byte(text, lo);
            let hi_b = char_to_byte(text, hi);
            if lo_b > 0 {
                job.append(&text[..lo_b], 0.0, fmt(regular));
            }
            if hi_b > lo_b {
                job.append(&text[lo_b..hi_b], 0.0, fmt(selected));
            }
            if hi_b < text.len() {
                job.append(&text[hi_b..], 0.0, fmt(regular));
            }
        } else {
            job.append(text, 0.0, fmt(regular));
        }
        ui.fonts(|f| f.layout_job(job))
    }
}
