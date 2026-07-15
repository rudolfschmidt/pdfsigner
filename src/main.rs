//! pdfsigner — minimalist PDF signing tool with the same aesthetic as
//! zathura/feh: black bars, monospace text, hotkey-driven, drag-and-drop
//! signatures. See README for usage.

use anyhow::{Context, Result};
use chrono::Local;
use eframe::egui;
use pdfium_render::prelude::Pdfium;
use std::collections::HashMap;
use std::path::PathBuf;

mod editor;
mod menus;
mod overlay;
mod pdf;
mod signatures;
mod theme;

use crate::overlay::{color_from_rgb, hit_test, overlay_rect, screen_to_pdf, selection_layouter, Overlay};

// ----------------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------------

const DEFAULT_IMG_WIDTH_PT: f32 = 150.0;
const DUPLICATE_OFFSET_PT: f32 = 10.0;
const WHEEL_PAGE_THRESHOLD: f32 = 60.0;
const TEXT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
const IMG_WIDTH_RANGE: (f32, f32) = (10.0, 600.0);
const IMG_RESIZE_FACTOR: f32 = 1.0625;

// ----------------------------------------------------------------------------
// Entry point
// ----------------------------------------------------------------------------

fn main() -> eframe::Result {
    let cli_pdf = std::env::args().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1300.0, 900.0])
            .with_title("pdfsigner")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "pdfsigner",
        options,
        Box::new(move |cc| {
            theme::install_fonts(&cc.egui_ctx);
            let mut app = App::new();
            if let Some(p) = cli_pdf
                && let Err(e) = app.open_pdf(p) {
                    eprintln!("[cli] open failed: {e:#}");
                    app.status = format!("Error: {e:#}");
                }
            Ok(Box::new(app))
        }),
    )
}

// ----------------------------------------------------------------------------
// Per-page data
// ----------------------------------------------------------------------------

pub(crate) struct PageData {
    pub size_pt: (f32, f32),
    pub image: egui::ColorImage,
    pub texture: Option<egui::TextureHandle>,
    pub overlays: Vec<Overlay>,
}

// ----------------------------------------------------------------------------
// App state
// ----------------------------------------------------------------------------

pub struct App {
    // pdfium binding (None ⇒ system library missing).
    pdfium: Option<Pdfium>,

    // Document.
    pub(crate) pdf_path: Option<PathBuf>,
    pub(crate) pages: Vec<PageData>,
    pub(crate) current: usize,

    // Selection + drag.
    pub(crate) selected: Vec<usize>,
    drag_offsets: Vec<(usize, egui::Vec2)>,
    rubber_band: Option<egui::Pos2>,

    // Inline text editor for `pages[current].overlays[editing]`.
    pub(crate) editing: Option<usize>,
    pub(crate) editing_just_focused: bool,
    pub(crate) edit_cursor_range: Option<egui::text_selection::CursorRange>,

    // Defaults for newly-spawned text overlays.
    pub(crate) text_size: f32,
    pub(crate) text_color: [u8; 3],

    // Signature library (`~/.config/pdfsigner/signatures/*.png`).
    pub(crate) signatures: Vec<PathBuf>,

    // Header pages-filter input.
    pub(crate) pages_filter: String,
    pub(crate) pages_filter_range: Option<egui::text_selection::CursorRange>,

    // Misc.
    wheel_accum: f32,
    pub(crate) status: String,
    image_textures: HashMap<PathBuf, egui::TextureHandle>,

    // Popups.
    pub(crate) sig_menu: Option<egui::Pos2>,
    pub(crate) color_menu: Option<egui::Pos2>,
    pub(crate) color_menu_consume_click: bool,
    pub(crate) color_custom: Option<egui::Pos2>,

    // Hotkey cheat-sheet overlay (toggled by `h`; modal while open).
    pub(crate) help_open: bool,

    // Block mode: empty-space drags create black redaction rectangles.
    // Toggled by `b`.
    pub(crate) block_mode: bool,
    // Mark mode: empty-space drags create rose-tinted pending marks that the
    // user later commits to black via `b`. Toggled by `Shift+B`.
    pub(crate) mark_mode: bool,
    // Redact visibility toggle. Default true: Redacts render fully opaque
    // black (exactly like the saved file). Toggled by `r` — when off, they
    // are not painted at all so the user can read the covered text.
    // Saving is unaffected (always writes the Redacts).
    pub(crate) show_redacts: bool,
}

// ----------------------------------------------------------------------------
// App: lifecycle (new, open, save)
// ----------------------------------------------------------------------------

impl App {
    fn new() -> Self {
        let pdfium = pdf::try_pdfium();
        let status = if pdfium.is_some() {
            String::new()
        } else {
            "ERROR: pdfium library not found. Install pdfium-binaries.".into()
        };
        Self {
            pdfium,
            pdf_path: None,
            pages: vec![],
            current: 0,
            selected: vec![],
            drag_offsets: vec![],
            rubber_band: None,
            editing: None,
            editing_just_focused: false,
            edit_cursor_range: None,
            text_size: 12.0,
            text_color: [0, 0, 0],
            signatures: signatures::list_signatures(),
            pages_filter: String::new(),
            pages_filter_range: None,
            wheel_accum: 0.0,
            status,
            image_textures: HashMap::new(),
            sig_menu: None,
            color_menu: None,
            color_menu_consume_click: false,
            color_custom: None,
            help_open: false,
            block_mode: false,
            mark_mode: false,
            show_redacts: true,
        }
    }

    fn open_pdf(&mut self, path: PathBuf) -> Result<()> {
        let pdfium = self.pdfium.as_ref().context("pdfium not loaded")?;
        let loaded = pdf::load_pages(pdfium, &path)?;
        self.pages = loaded
            .into_iter()
            .map(|p| PageData {
                size_pt: p.size_pt,
                image: p.image,
                texture: None,
                overlays: vec![],
            })
            .collect();
        self.pdf_path = Some(path);
        self.current = 0;
        self.selected.clear();
        self.image_textures.clear();
        self.status.clear();
        Ok(())
    }

    fn save(&self) -> Result<PathBuf> {
        let input = self.pdf_path.as_ref().context("no input PDF")?;
        let overlays: Vec<Vec<Overlay>> = self.pages.iter().map(|p| p.overlays.clone()).collect();
        let sizes: Vec<(f32, f32)> = self.pages.iter().map(|p| p.size_pt).collect();
        pdf::save(self.pdfium.as_ref(), input, &overlays, &sizes, &self.pages_filter)
    }
}

// ----------------------------------------------------------------------------
// App: overlay manipulation (called from menus, editor, hotkeys)
// ----------------------------------------------------------------------------

impl App {
    /// Spawn a text overlay; returns its index in the current page. The
    /// overlay is auto-selected.
    pub(crate) fn add_text_at(&mut self, text: String, pdf_x: f32, pdf_y: f32) -> usize {
        let size = self.text_size;
        // Anchor the cursor position at the text baseline. For ascender-only
        // characters (x, digits, dates) the baseline is the visual bottom of
        // the glyph → the cursor lands exactly where the ink starts, which
        // matters for stamping small checkboxes precisely.
        let y_top = pdf_y - size * theme::INTER_BASELINE_RATIO;
        let page = &mut self.pages[self.current];
        page.overlays.push(Overlay::Text {
            text,
            x: pdf_x,
            y: y_top,
            size_pt: size,
            color: self.text_color,
        });
        let idx = page.overlays.len() - 1;
        self.selected = vec![idx];
        idx
    }

    /// Spawn a signature image centred on `(pdf_x, pdf_y)`. Width is fixed
    /// at `DEFAULT_IMG_WIDTH_PT`; height keeps aspect ratio.
    pub(crate) fn add_signature_at(&mut self, path: PathBuf, pdf_x: f32, pdf_y: f32) {
        let Some(img) = self.open_image_to_status(&path) else { return };
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let w = DEFAULT_IMG_WIDTH_PT;
        let h = w * ih / iw;
        let page = &mut self.pages[self.current];
        page.overlays.push(Overlay::Image {
            path,
            x: pdf_x - w / 2.0,
            y: pdf_y - h / 2.0,
            w,
            h,
        });
        self.selected = vec![page.overlays.len() - 1];
    }

    /// Spawn an image overlay centred on the current page (used for image
    /// drops onto the canvas).
    fn add_image_centered(&mut self, path: PathBuf) {
        let Some(img) = self.open_image_to_status(&path) else { return };
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let w = DEFAULT_IMG_WIDTH_PT;
        let h = w * ih / iw;
        let page = &mut self.pages[self.current];
        let x = (page.size_pt.0 - w) / 2.0;
        let y = (page.size_pt.1 - h) / 2.0;
        page.overlays.push(Overlay::Image { path, x, y, w, h });
        self.selected = vec![page.overlays.len() - 1];
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        let mut idxs = std::mem::take(&mut self.selected);
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        if let Some(page) = self.pages.get_mut(self.current) {
            for i in idxs {
                if i < page.overlays.len() {
                    page.overlays.remove(i);
                }
            }
        }
    }

    fn duplicate_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        let cur = self.current;
        let Some(page) = self.pages.get_mut(cur) else { return };
        let mut new_indices = vec![];
        for &i in &self.selected {
            let Some(original) = page.overlays.get(i).cloned() else { continue };
            let mut dup = original;
            let (x, y) = dup.position_mut();
            *x += DUPLICATE_OFFSET_PT;
            *y += DUPLICATE_OFFSET_PT;
            page.overlays.push(dup);
            new_indices.push(page.overlays.len() - 1);
        }
        self.selected = new_indices;
    }

    /// `dir > 0` grows, `dir < 0` shrinks. Text is adjusted in pt-deltas;
    /// images by a constant geometric factor. With nothing selected the
    /// default text size for new spawns is adjusted instead.
    pub(crate) fn adjust_size(&mut self, dir: f32) {
        let cur = self.current;
        let mut applied = false;
        if let Some(page) = self.pages.get_mut(cur) {
            for &i in &self.selected.clone() {
                let Some(o) = page.overlays.get_mut(i) else { continue };
                match o {
                    Overlay::Text { size_pt, .. } => {
                        *size_pt = (*size_pt + dir).clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
                    }
                    Overlay::Image { w, h, .. } => {
                        let factor = if dir > 0.0 { IMG_RESIZE_FACTOR } else { 1.0 / IMG_RESIZE_FACTOR };
                        let new_w = (*w * factor).clamp(IMG_WIDTH_RANGE.0, IMG_WIDTH_RANGE.1);
                        *h *= new_w / *w;
                        *w = new_w;
                    }
                    Overlay::Redact { w, h, .. } | Overlay::PendingMark { w, h, .. } => {
                        // Grow/shrink from the top-left corner proportionally.
                        let factor = if dir > 0.0 { IMG_RESIZE_FACTOR } else { 1.0 / IMG_RESIZE_FACTOR };
                        *w = (*w * factor).max(4.0);
                        *h = (*h * factor).max(4.0);
                    }
                }
                applied = true;
            }
        }
        if !applied {
            self.text_size = (self.text_size + dir).clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
        }
    }

    /// Apply `color` to every selected overlay that is a text. Image
    /// overlays in the selection are left alone.
    pub(crate) fn apply_color_to_selected_text(&mut self, color: [u8; 3]) {
        let cur = self.current;
        for &i in &self.selected.clone() {
            if let Some(Overlay::Text { color: c, .. }) = self.pages[cur].overlays.get_mut(i) {
                *c = color;
            }
        }
    }

    /// Colour of the first selected text overlay, if any.
    pub(crate) fn first_selected_text_color(&self) -> Option<[u8; 3]> {
        self.selected.iter().find_map(|&i| {
            if let Some(Overlay::Text { color, .. }) = self.pages[self.current].overlays.get(i) {
                Some(*color)
            } else {
                None
            }
        })
    }

    /// Index of the text overlay currently being edited, or — if none — of
    /// the first selected text overlay. Used to drive the header pt-indicator.
    fn active_text_idx(&self) -> Option<usize> {
        self.editing.or_else(|| {
            self.selected.first().copied().filter(|&i| {
                matches!(self.pages[self.current].overlays.get(i), Some(Overlay::Text { .. }))
            })
        })
    }

    fn open_image_to_status(&mut self, path: &std::path::Path) -> Option<image::DynamicImage> {
        match image::open(path) {
            Ok(img) => Some(img),
            Err(e) => {
                self.status = format!("Image error: {e}");
                None
            }
        }
    }

    fn ensure_image_texture(&mut self, ctx: &egui::Context, path: &PathBuf) {
        if self.image_textures.contains_key(path) {
            return;
        }
        if let Ok(img) = image::open(path) {
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let ci = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
            let tex = ctx.load_texture(format!("img-{}", path.display()), ci, egui::TextureOptions::LINEAR);
            self.image_textures.insert(path.clone(), tex);
        }
    }
}

// ----------------------------------------------------------------------------
// eframe::App
// ----------------------------------------------------------------------------

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.style_mut(theme::apply_global);
        self.handle_dropped_files(ctx);
        self.handle_global_hotkeys(ctx);
        self.handle_wheel(ctx);
        self.render_header(ctx);
        self.render_footer(ctx);
        self.render_central(ctx);
        menus::render_help(self, ctx);
    }
}

// ----------------------------------------------------------------------------
// App: input handlers (run before any panel renders)
// ----------------------------------------------------------------------------

impl App {
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });
        for path in paths {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase());
            match ext.as_deref() {
                Some("pdf") => {
                    if let Err(e) = self.open_pdf(path) {
                        self.status = format!("Error: {e:#}");
                    }
                }
                Some("png" | "jpg" | "jpeg") if !self.pages.is_empty() => self.add_image_centered(path),
                Some("png" | "jpg" | "jpeg") => {
                    self.status = "Drop a PDF first, then drop the signature image.".into();
                }
                _ => self.status = format!("Unsupported file type: {}", path.display()),
            }
        }
    }

    fn handle_global_hotkeys(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing {
            let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
            // `h` toggles the cheat-sheet. While it's open it captures input
            // modally — `h`/`Esc`/`q` close it and nothing else fires.
            if pressed(egui::Key::H) {
                self.help_open = !self.help_open;
            }
            if self.help_open {
                if pressed(egui::Key::Escape) || pressed(egui::Key::Q) {
                    self.help_open = false;
                }
                return;
            }
            if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
                self.delete_selected();
            }
            // Zathura-style quit.
            if pressed(egui::Key::Q) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            // +/- adjust size (selection if any, else the default for new text).
            let dir = ctx.input(|i| {
                let mut d = 0.0_f32;
                for e in &i.events {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
                        if modifiers.ctrl || modifiers.command || modifiers.alt {
                            continue;
                        }
                        match key {
                            egui::Key::Plus | egui::Key::Equals => d += 1.0,
                            egui::Key::Minus => d -= 1.0,
                            _ => {}
                        }
                    }
                }
                d
            });
            if dir != 0.0 {
                self.adjust_size(dir);
            }
        }
        // Modifier-bound shortcuts work even while typing.
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) && !self.pages.is_empty() {
            match self.save() {
                Ok(out) => self.status = format!("Saved {}", out.display()),
                Err(e) => self.status = format!("Save error: {e:#}"),
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::D)) {
            self.duplicate_selected();
        }
    }

    /// With a selection: wheel resizes (sign of scroll is enough). Without a
    /// selection: accumulator-based page navigation using the unit-converted
    /// `raw_scroll_delta` (egui smooths this for us).
    fn handle_wheel(&mut self, ctx: &egui::Context) {
        if self.pages.is_empty() || self.help_open {
            return;
        }
        let (event_dy, raw_dy) = ctx.input(|i| {
            let mut ev = 0.0_f32;
            for e in &i.events {
                if let egui::Event::MouseWheel { delta, modifiers, .. } = e {
                    if modifiers.ctrl || modifiers.command {
                        continue;
                    }
                    ev += delta.y;
                }
            }
            (ev, i.raw_scroll_delta.y)
        });
        if !self.selected.is_empty() {
            if event_dy != 0.0 {
                self.adjust_size(event_dy.signum());
            }
            self.wheel_accum = 0.0;
            return;
        }
        if raw_dy == 0.0 {
            return;
        }
        self.wheel_accum += raw_dy;
        while self.wheel_accum >= WHEEL_PAGE_THRESHOLD {
            self.wheel_accum -= WHEEL_PAGE_THRESHOLD;
            if self.current > 0 {
                self.current -= 1;
                self.selected.clear();
            }
        }
        while self.wheel_accum <= -WHEEL_PAGE_THRESHOLD {
            self.wheel_accum += WHEEL_PAGE_THRESHOLD;
            if self.current + 1 < self.pages.len() {
                self.current += 1;
                self.selected.clear();
            }
        }
    }
}

// ----------------------------------------------------------------------------
// App: header / footer / central panels
// ----------------------------------------------------------------------------

impl App {
    fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(theme::black_panel_frame())
            .show_separator_line(false)
            .show(ctx, |ui| {
                theme::apply_header(ui.style_mut());
                let font = theme::bar_font();
                let pt_text = self.active_text_idx().and_then(|idx| {
                    if let Some(Overlay::Text { size_pt, .. }) =
                        self.pages[self.current].overlays.get(idx)
                    {
                        Some(format!("{size_pt:.0} pt"))
                    } else {
                        None
                    }
                });
                let sel_text =
                    (self.selected.len() > 1).then(|| format!("{} selected", self.selected.len()));

                ui.horizontal(|ui| {
                    let mut segments: Vec<(String, egui::Color32)> = vec![];
                    if self.block_mode {
                        segments.push(("BLOCK".into(), theme::ACCENT));
                    }
                    if self.mark_mode {
                        let pending = self.pages[self.current]
                            .overlays
                            .iter()
                            .filter(|o| o.is_pending_mark())
                            .count();
                        segments.push((format!("MARK · {pending} pending"), theme::ACCENT));
                    }
                    if let Some(s) = pt_text.as_ref() {
                        segments.push((s.clone(), egui::Color32::WHITE));
                    }
                    if let Some(s) = sel_text.as_ref() {
                        segments.push((s.clone(), egui::Color32::WHITE));
                    }
                    for (i, (s, c)) in segments.iter().enumerate() {
                        if i > 0 {
                            ui.label(label("·".into(), theme::SEPARATOR_DOT_GRAY, font.clone()));
                        }
                        ui.label(label(s.clone(), *c, font.clone()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut layouter = selection_layouter(
                            self.pages_filter_range,
                            font.clone(),
                            egui::Color32::WHITE,
                            egui::Color32::BLACK,
                        );
                        let output = egui::TextEdit::singleline(&mut self.pages_filter)
                            .hint_text(label("output: 1-3,5  empty=all".into(), theme::PLACEHOLDER_GRAY, font.clone()))
                            .text_color(egui::Color32::WHITE)
                            .desired_width(185.0)
                            .font(font.clone())
                            .layouter(&mut layouter)
                            .show(ui);
                        self.pages_filter_range = output.cursor_range;
                    });
                });
            });
    }

    fn render_footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer")
            .frame(theme::black_panel_frame())
            .show_separator_line(false)
            .show(ctx, |ui| {
                let font = theme::bar_font();

                let path = self
                    .pdf_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(no PDF)".into());
                let pages = if self.pages.is_empty() {
                    String::new()
                } else {
                    format!("[{}/{}]", self.current + 1, self.pages.len())
                };

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !pages.is_empty() {
                        ui.label(label(pages, egui::Color32::WHITE, font.clone()));
                    }
                    if !self.status.is_empty() {
                        let is_error = self.status.to_lowercase().contains("error");
                        let color = if is_error { theme::ERROR_RED } else { egui::Color32::WHITE };
                        ui.add_space(12.0);
                        ui.label(label(self.status.clone(), color, font.clone()));
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(label(path, egui::Color32::WHITE, font.clone()));
                    });
                });
            });
    }

    fn render_central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                if self.pages.is_empty() {
                    self.draw_empty_hint(ui);
                    return;
                }

                // Pre-load image textures for the current page.
                let image_paths: Vec<PathBuf> = self.pages[self.current]
                    .overlays
                    .iter()
                    .filter_map(|o| match o {
                        Overlay::Image { path, .. } => Some(path.clone()),
                        _ => None,
                    })
                    .collect();
                for p in &image_paths {
                    self.ensure_image_texture(ctx, p);
                }

                // Lazy-load the page texture.
                let cur = self.current;
                if self.pages[cur].texture.is_none() {
                    let img = self.pages[cur].image.clone();
                    let tex = ctx.load_texture(format!("page-{cur}"), img, egui::TextureOptions::LINEAR);
                    self.pages[cur].texture = Some(tex);
                }
                self.draw_page(ui, ctx);
            });
    }

    fn draw_empty_hint(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.35);
            ui.label(egui::RichText::new("No PDF loaded").size(28.0).strong());
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Drop a PDF here, or pass one as CLI argument.")
                    .size(18.0)
                    .color(egui::Color32::GRAY),
            );
        });
    }
}

// ----------------------------------------------------------------------------
// App: page rendering and per-page input handling
// ----------------------------------------------------------------------------

impl App {
    fn draw_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let cur = self.current;
        let size_pt = self.pages[cur].size_pt;
        let texture = self.pages[cur].texture.as_ref().unwrap().id();

        // Centre the page within the available area (zathura-style).
        let avail = ui.available_rect_before_wrap();
        let scale = (avail.width() / size_pt.0)
            .min(avail.height() / size_pt.1)
            .max(0.1);
        let display_size = egui::vec2(size_pt.0 * scale, size_pt.1 * scale);
        let page_rect = egui::Rect::from_center_size(avail.center(), display_size);
        let response = ui.allocate_rect(page_rect, egui::Sense::click_and_drag());
        let painter = ui.painter_at(page_rect);

        // 1. Page texture.
        painter.rect_filled(page_rect, 0.0, egui::Color32::WHITE);
        painter.image(
            texture,
            page_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // 2. Overlays + selection outlines.
        let overlay_rects: Vec<egui::Rect> = self.pages[cur]
            .overlays
            .iter()
            .map(|o| overlay_rect(o, page_rect, scale, ctx))
            .collect();
        self.draw_overlays(&painter, &overlay_rects, scale);

        // 3. Marquee selection box (drawn on top of overlays). In block mode
        // preview as semi-opaque black; in mark mode as translucent rose.
        if let Some(start) = self.rubber_band {
            let end = response.interact_pointer_pos().unwrap_or(start);
            let band = egui::Rect::from_two_pos(start, end);
            let (fill, stroke) = if self.block_mode {
                (
                    egui::Color32::from_rgba_premultiplied(0, 0, 0, 80),
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 180)),
                )
            } else if self.mark_mode {
                (
                    egui::Color32::from_rgba_premultiplied(220, 80, 80, 70),
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(220, 80, 80, 200)),
                )
            } else {
                (theme::RUBBER_FILL, egui::Stroke::new(1.5, theme::RUBBER_STROKE))
            };
            painter.rect(band, 0.0, fill, stroke);
        }

        // 4. Pointer (drag / click) — gated by open menus.
        let consume_click = std::mem::take(&mut self.color_menu_consume_click);
        let block_primary =
            self.color_menu.is_some() || self.color_custom.is_some() || consume_click || self.help_open;
        if !block_primary {
            self.handle_pointer(&response, ctx, page_rect, scale, &overlay_rects);
        }

        // 5. Right-click sig menu (open + render + release).
        let (rclick_pressed, rclick_released) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.pointer.button_released(egui::PointerButton::Secondary),
            )
        });
        if rclick_pressed && !self.signatures.is_empty() && !self.help_open
            && let Some(pos) = response.hover_pos()
                && page_rect.contains(pos) {
                    self.sig_menu = Some(pos);
                }
        menus::render_sig_menu(self, ctx, page_rect, scale, rclick_released);
        menus::render_color_menu(self, ctx);
        menus::render_color_custom(self, ctx);

        // 6. Inline text editor (renders when `self.editing` is `Some`).
        editor::render_inline_editor(self, ctx, ui, page_rect, scale);

        // 7. Mouse-anchored hotkeys (s/Shift+S/x/t/d/c/b/Shift+B/r). Skipped
        // while typing.
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && !self.help_open {
            self.handle_page_hotkeys(ctx, &response, page_rect, scale, size_pt);
        }
    }

    fn draw_overlays(&self, painter: &egui::Painter, rects: &[egui::Rect], scale: f32) {
        let cur = self.current;
        for (i, overlay) in self.pages[cur].overlays.iter().enumerate() {
            let r = rects[i];
            // The overlay being edited is drawn as a TextEdit further below;
            // skip its content here, but draw its selection outline so the
            // box doesn't disappear / jump on enter/exit edit.
            if Some(i) == self.editing {
                if self.selected.contains(&i) {
                    painter.rect_stroke(r.expand(2.0), 0.0, egui::Stroke::new(2.0, theme::ACCENT));
                }
                continue;
            }
            match overlay {
                Overlay::Image { path, .. } => {
                    if let Some(tex) = self.image_textures.get(path) {
                        painter.image(
                            tex.id(),
                            r,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    } else {
                        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::DARK_GRAY));
                    }
                }
                Overlay::Text { text, size_pt, color, .. } => {
                    painter.text(
                        r.min,
                        egui::Align2::LEFT_TOP,
                        text,
                        egui::FontId::proportional(size_pt * scale),
                        color_from_rgb(*color),
                    );
                }
                Overlay::Redact { .. } => {
                    if self.show_redacts {
                        painter.rect_filled(r, 0.0, egui::Color32::BLACK);
                    }
                    // else: skip drawing entirely — user is peeking at the
                    // covered text. Save still writes the redaction.
                }
                Overlay::PendingMark { .. } => {
                    // Rose tint on the accent-red palette — matches the app's
                    // red selection theme and reads as "pending, awaiting
                    // commit" without the yellow-marker garishness.
                    painter.rect_filled(
                        r,
                        0.0,
                        egui::Color32::from_rgba_premultiplied(220, 80, 80, 55),
                    );
                }
            }
            if self.selected.contains(&i) {
                painter.rect_stroke(r.expand(2.0), 0.0, egui::Stroke::new(2.0, theme::ACCENT));
            }
        }
    }

    fn handle_pointer(
        &mut self,
        response: &egui::Response,
        ctx: &egui::Context,
        page_rect: egui::Rect,
        scale: f32,
        rects: &[egui::Rect],
    ) {
        let cur = self.current;

        if response.drag_started()
            // Use press_origin (the click point) rather than
            // interact_pointer_pos which reflects the current — already-moved —
            // cursor after egui's drag_threshold has been exceeded.
            && let Some(pos) = ctx
                .input(|i| i.pointer.press_origin())
                .or_else(|| response.interact_pointer_pos()) {
                match hit_test(rects, pos) {
                    Some(i) => {
                        if !self.selected.contains(&i) {
                            self.selected = vec![i];
                        }
                        self.drag_offsets = self
                            .selected
                            .iter()
                            .filter_map(|&j| rects.get(j).map(|r| (j, pos - r.left_top())))
                            .collect();
                        self.rubber_band = None;
                    }
                    None => {
                        // Empty drag → marquee select.
                        self.selected.clear();
                        self.drag_offsets.clear();
                        self.rubber_band = Some(pos);
                    }
                }
            }

        if response.dragged() && !self.drag_offsets.is_empty()
            && let Some(pos) = response.interact_pointer_pos() {
                for &(idx, off) in &self.drag_offsets {
                    let (pdf_x, pdf_y) = screen_to_pdf(pos - off, page_rect, scale);
                    if let Some(o) = self.pages[cur].overlays.get_mut(idx) {
                        let (x, y) = o.position_mut();
                        *x = pdf_x;
                        *y = pdf_y;
                    }
                }
            }

        if response.drag_stopped() {
            if let Some(start) = self.rubber_band.take() {
                let end = response.interact_pointer_pos().unwrap_or(start);
                let band = egui::Rect::from_two_pos(start, end);
                let too_small = band.width() < 3.0 || band.height() < 3.0;
                if self.block_mode && !too_small {
                    let (x, y, w, h) = pdf_coords_from_band(band, page_rect, scale);
                    self.pages[cur].overlays.push(Overlay::Redact { x, y, w, h });
                    merge_overlapping_redacts(&mut self.pages[cur].overlays);
                    // Single-shot: exit block mode after one rectangle. Press
                    // `b` again for another one.
                    self.block_mode = false;
                } else if self.mark_mode && !too_small {
                    let (x, y, w, h) = pdf_coords_from_band(band, page_rect, scale);
                    let detected = match (self.pdfium.as_ref(), self.pdf_path.as_ref()) {
                        (Some(pdfium), Some(path)) => {
                            pdf::detect_text_lines(pdfium, path, cur, (x, y, w, h))
                        }
                        _ => vec![],
                    };
                    if detected.is_empty() {
                        self.pages[cur].overlays.push(Overlay::PendingMark { x, y, w, h });
                        self.status = "No text detected — using rectangle.".into();
                    } else {
                        for (mx, my, mw, mh) in detected {
                            self.pages[cur].overlays.push(Overlay::PendingMark {
                                x: mx,
                                y: my,
                                w: mw,
                                h: mh,
                            });
                        }
                    }
                    // Any overlap with existing PendingMarks → merge into a
                    // single union rect. Guarantees an area is marked once.
                    merge_overlapping_marks(&mut self.pages[cur].overlays);
                } else if !self.block_mode && !self.mark_mode {
                    self.selected = rects
                        .iter()
                        .enumerate()
                        .filter_map(|(i, r)| band.intersects(*r).then_some(i))
                        .collect();
                }
            }
            self.drag_offsets.clear();
        }

        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos() {
                let hit = hit_test(rects, pos);
                let ctrl = ctx.input(|i| i.modifiers.ctrl);
                match hit {
                    Some(i) if ctrl => {
                        if let Some(p) = self.selected.iter().position(|&x| x == i) {
                            self.selected.remove(p);
                        } else {
                            self.selected.push(i);
                        }
                    }
                    Some(i) => {
                        self.selected = vec![i];
                        // Click on a text overlay → enter inline edit.
                        if matches!(self.pages[cur].overlays.get(i), Some(Overlay::Text { .. })) {
                            self.editing = Some(i);
                            self.editing_just_focused = false;
                        }
                    }
                    None => self.selected.clear(),
                }
            }
    }

    /// Hotkeys that act at the mouse position over the page area:
    /// - `s` / `S` — stamp today's date in DE / US format.
    /// - `x`       — insert literal "x".
    /// - `t`       — spawn an empty text overlay and enter inline edit.
    /// - `d`       — delete current selection.
    /// - `c`       — toggle the color picker for the selected text overlay(s).
    fn handle_page_hotkeys(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        page_rect: egui::Rect,
        scale: f32,
        size_pt: (f32, f32),
    ) {
        let Some(pos) = response.hover_pos() else { return };
        let (pdf_x, pdf_y) = screen_to_pdf(pos, page_rect, scale);
        let in_page = (0.0..=size_pt.0).contains(&pdf_x) && (0.0..=size_pt.1).contains(&pdf_y);
        if !in_page {
            return;
        }

        let events = ctx.input(|i| i.events.clone());
        let cur = self.current;
        for ev in events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = ev
            else {
                continue;
            };
            if modifiers.ctrl || modifiers.command || modifiers.alt {
                continue;
            }

            // Stamp / literal text — no edit-mode entry.
            let stamped = match (key, modifiers.shift) {
                (egui::Key::S, false) => Some(Local::now().format("%d.%m.%Y").to_string()),
                (egui::Key::S, true) => Some(Local::now().format("%m/%d/%Y").to_string()),
                (egui::Key::X, false) => Some("x".to_string()),
                _ => None,
            };
            if let Some(text) = stamped {
                self.add_text_at(text, pdf_x, pdf_y);
                continue;
            }

            match key {
                egui::Key::T if !modifiers.shift => {
                    let idx = self.add_text_at(String::new(), pdf_x, pdf_y);
                    self.editing = Some(idx);
                    self.editing_just_focused = false;
                }
                egui::Key::D if !modifiers.shift => self.delete_selected(),
                egui::Key::C if !modifiers.shift
                    && self.first_selected_text_color().is_some() =>
                {
                    self.color_menu = if self.color_menu.is_some() { None } else { Some(pos) };
                }
                egui::Key::B if modifiers.shift && !self.block_mode => {
                    // Shift+B toggles mark mode. Ignored while block mode is
                    // active — user must Esc out of block mode first.
                    self.mark_mode = !self.mark_mode;
                    self.selected.clear();
                }
                egui::Key::B if !modifiers.shift => {
                    // `b` always means "make it black" — context-aware:
                    // - In mark mode: commit every PendingMark to a Redact.
                    //   Stay in mark mode so the user can keep selecting.
                    // - Otherwise: toggle block mode (single-shot drag).
                    if self.mark_mode {
                        let overlays = &mut self.pages[cur].overlays;
                        for o in overlays.iter_mut() {
                            if let Overlay::PendingMark { x, y, w, h } = *o {
                                *o = Overlay::Redact { x, y, w, h };
                            }
                        }
                        merge_overlapping_redacts(&mut self.pages[cur].overlays);
                    } else {
                        self.block_mode = !self.block_mode;
                        self.selected.clear();
                    }
                }
                egui::Key::Escape if self.block_mode || self.mark_mode => {
                    // Leave whichever mode is on and discard pending marks
                    // (which are UI-only anyway).
                    self.block_mode = false;
                    self.mark_mode = false;
                    self.pages[cur].overlays.retain(|o| !o.is_pending_mark());
                }
                egui::Key::R if !modifiers.shift => {
                    // Toggle Redact visibility. Off = peek at the text
                    // underneath. Saving always writes the Redacts either way.
                    self.show_redacts = !self.show_redacts;
                }
                _ => {}
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

fn label(text: String, color: egui::Color32, font: egui::FontId) -> egui::RichText {
    egui::RichText::new(text).color(color).font(font)
}

/// Fold overlays of the same "kind" (extracted via `extract`) that share any
/// positive area with each other into a single union rectangle. Guarantees no
/// two rects of that kind overlap afterwards, so the visual result of marking
/// the same area N times is identical to marking it once (no alpha stacking,
/// no double-processing).
fn merge_overlapping<K, M>(overlays: &mut Vec<Overlay>, extract: K, make: M)
where
    K: Fn(&Overlay) -> Option<(f32, f32, f32, f32)>,
    M: Fn(f32, f32, f32, f32) -> Overlay,
{
    let mut rects: Vec<(f32, f32, f32, f32)> = vec![];
    overlays.retain(|o| match extract(o) {
        Some(r) => {
            rects.push(r);
            false
        }
        None => true,
    });
    loop {
        let mut merged = None;
        'search: for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                if intersection_area(rects[i], rects[j]) > 0.0 {
                    merged = Some((i, j, union_rect(rects[i], rects[j])));
                    break 'search;
                }
            }
        }
        match merged {
            Some((i, j, u)) => {
                rects[i] = u;
                rects.remove(j);
            }
            None => break,
        }
    }
    for (x, y, w, h) in rects {
        overlays.push(make(x, y, w, h));
    }
}

fn merge_overlapping_redacts(overlays: &mut Vec<Overlay>) {
    merge_overlapping(
        overlays,
        |o| if let Overlay::Redact { x, y, w, h } = *o { Some((x, y, w, h)) } else { None },
        |x, y, w, h| Overlay::Redact { x, y, w, h },
    );
}

fn merge_overlapping_marks(overlays: &mut Vec<Overlay>) {
    merge_overlapping(
        overlays,
        |o| if let Overlay::PendingMark { x, y, w, h } = *o { Some((x, y, w, h)) } else { None },
        |x, y, w, h| Overlay::PendingMark { x, y, w, h },
    );
}

fn intersection_area(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> f32 {
    let ox = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
    let oy = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
    ox.max(0.0) * oy.max(0.0)
}
fn union_rect(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let x = a.0.min(b.0);
    let y = a.1.min(b.1);
    let r = (a.0 + a.2).max(b.0 + b.2);
    let bo = (a.1 + a.3).max(b.1 + b.3);
    (x, y, r - x, bo - y)
}

/// Screen-space band → PDF-space (x, y, w, h) tuple.
fn pdf_coords_from_band(band: egui::Rect, page_rect: egui::Rect, scale: f32) -> (f32, f32, f32, f32) {
    let (x, y) = screen_to_pdf(band.min, page_rect, scale);
    (x, y, band.width() / scale, band.height() / scale)
}

