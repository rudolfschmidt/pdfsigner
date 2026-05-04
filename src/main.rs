use anyhow::{Context, Result};
use chrono::Local;
use eframe::egui;
use flate2::{write::ZlibEncoder, Compression};
use image::DynamicImage;
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ============================================================================
// Constants
// ============================================================================

const INTER_TTF: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
const INTER_ASCENT: f32 = 0.728; // cap height as fraction of font size
const INTER_DESCENT: f32 = 0.25;
const EGUI_BASELINE_FROM_TOP: f32 = 0.928; // baseline in egui galley, fraction of font size

const PAGE_RENDER_WIDTH: u32 = 1800;
const PAGE_RENDER_MAX_HEIGHT: u32 = 2400;

const DEFAULT_TEXT_POS: (f32, f32) = (60.0, 80.0);
const DEFAULT_IMG_WIDTH_PT: f32 = 150.0;
const DUPLICATE_OFFSET_PT: f32 = 10.0;
const WHEEL_PAGE_THRESHOLD: f32 = 60.0;

const SEL_STROKE: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);
const RUBBER_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 35, 60, 30);
const RUBBER_STROKE: egui::Color32 = egui::Color32::from_rgb(80, 140, 230);

// ============================================================================
// Types
// ============================================================================

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
        Box::new(move |_cc| {
            let mut app = App::new();
            if let Some(p) = cli_pdf {
                if let Err(e) = app.open_pdf(p) {
                    eprintln!("[cli] open failed: {e:#}");
                    app.status = format!("Error: {e:#}");
                } else {
                    app.status = format!("PDF loaded ({} pages).", app.pages.len());
                }
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(Clone)]
enum Overlay {
    Image { path: PathBuf, x: f32, y: f32, w: f32, h: f32 },
    Text { text: String, x: f32, y: f32, size_pt: f32, color: [u8; 3] },
}

struct PageData {
    page_pt: (f32, f32),
    image: egui::ColorImage,
    texture: Option<egui::TextureHandle>,
    overlays: Vec<Overlay>,
}

struct App {
    pdfium: Option<Pdfium>,
    pdf_path: Option<PathBuf>,
    pages: Vec<PageData>,
    current: usize,
    selected: Vec<usize>,
    drag_offsets: Vec<(usize, egui::Vec2)>,
    rubber_band: Option<egui::Pos2>,
    text_buf: String,
    text_size: f32,
    text_color: [u8; 3],
    signatures: Vec<PathBuf>,
    sig_choice: usize,
    pages_filter: String,
    wheel_accum: f32,
    status: String,
    image_textures: HashMap<PathBuf, egui::TextureHandle>,
}

/// Load a PDF tolerantly: first try lopdf strict; on failure round-trip
/// through pdfium to normalize the structure (decompress xref-streams,
/// unpack ObjStm) and try again. Handles Chrome/Skia output, compressed
/// xref tables, and incrementally-updated PDFs that lopdf can't parse.
fn load_pdf_robust(path: &Path, pdfium: Option<&Pdfium>) -> Result<Document> {
    match Document::load(path) {
        Ok(d) => return Ok(d),
        Err(e) => eprintln!("[load] strict load failed ({e}), normalizing via pdfium"),
    }
    let pdfium = pdfium.context("pdfium not loaded — cannot normalize broken PDF")?;
    let path_str = path.to_str().context("non-UTF8 PDF path")?;
    let doc = pdfium
        .load_pdf_from_file(path_str, None)
        .context("pdfium failed to load PDF")?;
    let bytes = doc.save_to_bytes().context("pdfium failed to serialize PDF")?;
    Document::load_mem(&bytes).context("loading PDF after pdfium normalization")
}

fn parse_page_range(s: &str, total: usize) -> Vec<usize> {
    let mut out: Vec<usize> = vec![];
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                for p in lo..=hi {
                    if (1..=total).contains(&p) && !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        } else if let Ok(p) = part.parse::<usize>() {
            if (1..=total).contains(&p) && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out.sort_unstable();
    out
}

fn signatures_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/pdfsigner/signatures"))
}

fn list_signatures() -> Vec<PathBuf> {
    let Some(dir) = signatures_dir() else { return vec![] };
    let _ = std::fs::create_dir_all(&dir);
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

impl App {
    fn new() -> Self {
        let pdfium = Pdfium::bind_to_system_library()
            .or_else(|_| Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./")))
            .ok()
            .map(Pdfium::new);
        let status = if pdfium.is_some() {
            "Drop a PDF here, or pass one as command-line argument.".to_string()
        } else {
            "ERROR: pdfium library not found. Install pdfium-binaries.".to_string()
        };
        Self {
            pdfium,
            pdf_path: None,
            pages: vec![],
            current: 0,
            selected: vec![],
            drag_offsets: vec![],
            rubber_band: None,
            text_buf: String::new(),
            text_size: 12.0,
            text_color: [0, 0, 0],
            signatures: list_signatures(),
            sig_choice: 0,
            pages_filter: String::new(),
            wheel_accum: 0.0,
            status,
            image_textures: HashMap::new(),
        }
    }

    fn open_pdf(&mut self, path: PathBuf) -> Result<()> {
        let pdfium = self.pdfium.as_ref().context("pdfium not loaded")?;
        let doc = pdfium
            .load_pdf_from_file(&path, None)
            .with_context(|| format!("loading {}", path.display()))?;
        let render_config = PdfRenderConfig::new()
            .set_target_width(PAGE_RENDER_WIDTH as i32)
            .set_maximum_height(PAGE_RENDER_MAX_HEIGHT as i32);

        let mut pages = vec![];
        for page in doc.pages().iter() {
            let pw = page.width().value;
            let ph = page.height().value;
            let bitmap = page.render_with_config(&render_config).context("rendering page")?;
            let rgba = bitmap.as_image().to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
            pages.push(PageData {
                page_pt: (pw, ph),
                image: color_image,
                texture: None,
                overlays: vec![],
            });
        }
        self.pages = pages;
        self.pdf_path = Some(path);
        self.current = 0;
        self.selected.clear();
        self.image_textures.clear();
        Ok(())
    }

    fn save_pdf(&self) -> Result<PathBuf> {
        let input = self.pdf_path.as_ref().context("no input PDF")?;
        let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "out".into());
        let parent = input.parent().unwrap_or(Path::new("."));
        let mut output = parent.join(format!("{stem}_signed.pdf"));
        let mut n = 1;
        while output.exists() {
            output = parent.join(format!("{stem}_signed_{n}.pdf"));
            n += 1;
        }

        let mut doc = load_pdf_robust(input, self.pdfium.as_ref())?;
        let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
        let total_pages = page_ids.len();
        let keep: Option<std::collections::HashSet<usize>> = {
            let s = self.pages_filter.trim();
            if s.is_empty() {
                None
            } else {
                let parsed: std::collections::HashSet<usize> =
                    parse_page_range(s, total_pages).into_iter().collect();
                if parsed.is_empty() {
                    return Err(anyhow::anyhow!("Pages filter '{s}' selects no pages"));
                }
                Some(parsed)
            }
        };
        let mut inter_font_id: Option<ObjectId> = None;

        for (idx, page_id) in page_ids.iter().enumerate() {
            let page_num = idx + 1;
            if let Some(k) = &keep {
                if !k.contains(&page_num) {
                    continue;
                }
            }
            let pd = match self.pages.get(idx) {
                Some(p) if !p.overlays.is_empty() => p,
                _ => continue,
            };
            let (_, ph) = pd.page_pt;

            let mut content: Vec<u8> = Vec::new();
            content.extend_from_slice(b"Q\n"); // close `q` prepended by wrap_and_append_overlay

            let mut images: Vec<(String, ObjectId)> = vec![];
            let mut text_used = false;

            for overlay in &pd.overlays {
                match overlay {
                    Overlay::Image { path, x, y, w, h } => {
                        let img = image::open(path).with_context(|| format!("opening image {}", path.display()))?;
                        let img_id = embed_image(&mut doc, &img)?;
                        let res_name = format!("Im{}", images.len() + 1);
                        let pdf_y = ph - y - h;
                        let line = format!("q\n{:.4} 0 0 {:.4} {:.4} {:.4} cm\n/{} Do\nQ\n", w, h, x, pdf_y, res_name);
                        content.extend_from_slice(line.as_bytes());
                        images.push((res_name, img_id));
                    }
                    Overlay::Text { text, x, y, size_pt, color } => {
                        text_used = true;
                        let baseline = y + size_pt * INTER_ASCENT;
                        let pdf_y = ph - baseline;
                        let r = color[0] as f32 / 255.0;
                        let g = color[1] as f32 / 255.0;
                        let b = color[2] as f32 / 255.0;
                        let header = format!(
                            "BT\n/F1 {:.4} Tf\n{:.4} {:.4} {:.4} rg\n{:.4} {:.4} Td\n",
                            size_pt, r, g, b, x, pdf_y
                        );
                        content.extend_from_slice(header.as_bytes());
                        content.extend_from_slice(b"<");
                        content.extend_from_slice(encode_text_for_inter(text).as_bytes());
                        content.extend_from_slice(b"> Tj\nET\n");
                    }
                }
            }

            let mut stream = Stream::new(dictionary! {}, content);
            let _ = stream.compress();
            let overlay_id = doc.add_object(stream);
            wrap_and_append_overlay(&mut doc, *page_id, overlay_id)?;

            if text_used {
                let font_id = match inter_font_id {
                    Some(id) => id,
                    None => {
                        let id = embed_inter_font(&mut doc)?;
                        inter_font_id = Some(id);
                        id
                    }
                };
                add_page_resource(&mut doc, *page_id, b"Font", b"F1", font_id)?;
            }
            for (name, img_id) in &images {
                add_page_resource(&mut doc, *page_id, b"XObject", name.as_bytes(), *img_id)?;
            }
        }

        if let Some(k) = &keep {
            let drop: Vec<u32> = (1..=total_pages as u32)
                .filter(|p| !k.contains(&(*p as usize)))
                .collect();
            if !drop.is_empty() {
                doc.delete_pages(&drop);
            }
        }

        doc.save(&output)?;
        Ok(output)
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

    fn add_image_overlay(&mut self, p: PathBuf) {
        let img = match image::open(&p) {
            Ok(i) => i,
            Err(e) => {
                self.status = format!("Image error: {e}");
                return;
            }
        };
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let w = DEFAULT_IMG_WIDTH_PT;
        let h = w * ih / iw;
        let page = &mut self.pages[self.current];
        let cx = (page.page_pt.0 - w) / 2.0;
        let cy = (page.page_pt.1 - h) / 2.0;
        page.overlays.push(Overlay::Image { path: p, x: cx, y: cy, w, h });
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
            if let Some(o) = page.overlays.get(i).cloned() {
                let mut dup = o;
                match &mut dup {
                    Overlay::Image { x, y, .. } | Overlay::Text { x, y, .. } => {
                        *x += DUPLICATE_OFFSET_PT;
                        *y += DUPLICATE_OFFSET_PT;
                    }
                }
                page.overlays.push(dup);
                new_indices.push(page.overlays.len() - 1);
            }
        }
        self.selected = new_indices;
    }

    fn add_text_overlay(&mut self, text: String) {
        let page = &mut self.pages[self.current];
        page.overlays.push(Overlay::Text {
            text,
            x: DEFAULT_TEXT_POS.0,
            y: DEFAULT_TEXT_POS.1,
            size_pt: self.text_size,
            color: self.text_color,
        });
        self.selected = vec![page.overlays.len() - 1];
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drag & drop
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });
        for path in dropped {
            let ext = path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase());
            match ext.as_deref() {
                Some("pdf") => match self.open_pdf(path) {
                    Ok(()) => self.status = format!("PDF loaded ({} pages).", self.pages.len()),
                    Err(e) => self.status = format!("Error: {e:#}"),
                },
                Some("png" | "jpg" | "jpeg") if !self.pages.is_empty() => self.add_image_overlay(path),
                Some("png" | "jpg" | "jpeg") => {
                    self.status = "Drop a PDF first, then drop the signature image.".into();
                }
                _ => self.status = format!("Unsupported file type: {}", path.display()),
            }
        }

        // Shortcuts (skip when typing in a text field)
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            self.delete_selected();
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) && !self.pages.is_empty() {
            match self.save_pdf() {
                Ok(out) => self.status = format!("Saved {}", out.display()),
                Err(e) => self.status = format!("Save error: {e:#}"),
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::D)) {
            self.duplicate_selected();
        }

        // Mouse wheel → page navigation. Accumulate to avoid jumping multiple pages per notch.
        if !self.pages.is_empty() {
            let dy = ctx.input(|i| i.raw_scroll_delta.y);
            if dy != 0.0 {
                self.wheel_accum += dy;
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

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            let style = ui.style_mut();
            style.spacing.button_padding = egui::vec2(12.0, 8.0);
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.interact_size.y = 32.0;
            style.text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Body,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let has_pdf = !self.pages.is_empty();

                // Signature picker — selecting an item inserts it immediately
                if has_pdf && !self.signatures.is_empty() {
                    let mut chosen: Option<PathBuf> = None;
                    egui::ComboBox::from_id_salt("sig_picker")
                        .selected_text("Insert signature")
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for (i, p) in self.signatures.iter().enumerate() {
                                let label = p
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_else(|| p.display().to_string());
                                if ui.selectable_label(false, label).clicked() {
                                    self.sig_choice = i;
                                    chosen = Some(p.clone());
                                }
                            }
                        });
                    if let Some(p) = chosen {
                        self.add_image_overlay(p);
                    }
                    ui.separator();
                }

                if ui
                    .add_enabled(has_pdf, egui::Button::new("DE"))
                    .on_hover_text("Today, German format (DD.MM.YYYY)")
                    .clicked()
                {
                    self.add_text_overlay(Local::now().format("%d.%m.%Y").to_string());
                }
                if ui
                    .add_enabled(has_pdf, egui::Button::new("US"))
                    .on_hover_text("Today, US format (MM/DD/YYYY)")
                    .clicked()
                {
                    self.add_text_overlay(Local::now().format("%m/%d/%Y").to_string());
                }
                if ui
                    .add_enabled(has_pdf, egui::Button::new("X"))
                    .on_hover_text("Insert an X (for checkboxes)")
                    .clicked()
                {
                    self.add_text_overlay("X".into());
                }

                // sel_kind: 'n'=none, 't'=single text, 'i'=single image, 'm'=multi
                let sel_kind = match self.selected.len() {
                    0 => 'n',
                    1 => match self.pages.get(self.current).and_then(|p| p.overlays.get(self.selected[0])) {
                        Some(Overlay::Text { .. }) => 't',
                        Some(Overlay::Image { .. }) => 'i',
                        None => 'n',
                    },
                    _ => 'm',
                };

                ui.label("Text:");
                if sel_kind == 't' {
                    let sel = self.selected[0];
                    if let Some(Overlay::Text { text, .. }) =
                        self.pages[self.current].overlays.get_mut(sel)
                    {
                        ui.add(
                            egui::TextEdit::singleline(text)
                                .hint_text("(empty)")
                                .desired_width(200.0),
                        );
                    }
                } else {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.text_buf)
                            .hint_text("type & enter…")
                            .desired_width(200.0),
                    );
                    let submit = resp.lost_focus()
                        && ctx.input(|i| i.key_pressed(egui::Key::Enter))
                        && !self.text_buf.is_empty();
                    let click_add = ui
                        .add_enabled(has_pdf && !self.text_buf.is_empty(), egui::Button::new("Add"))
                        .clicked();
                    if (submit || click_add) && has_pdf && !self.text_buf.is_empty() {
                        let txt = std::mem::take(&mut self.text_buf);
                        self.add_text_overlay(txt);
                    }
                }

                ui.separator();
                ui.label("Size:");
                if sel_kind == 't' {
                    let sel = self.selected[0];
                    if let Some(Overlay::Text { size_pt, color, .. }) =
                        self.pages[self.current].overlays.get_mut(sel)
                    {
                        ui.add(egui::DragValue::new(size_pt).range(6.0..=72.0).suffix(" pt"));
                        color_controls(ui, color);
                    }
                } else {
                    ui.add(egui::DragValue::new(&mut self.text_size).range(6.0..=72.0).suffix(" pt"));
                    color_controls(ui, &mut self.text_color);
                }

                if sel_kind == 'i' {
                    ui.separator();
                    let sel = self.selected[0];
                    if let Some(Overlay::Image { w, h, .. }) =
                        self.pages[self.current].overlays.get_mut(sel)
                    {
                        let aspect = *h / *w;
                        ui.label("Width:");
                        let mut nw = *w;
                        if ui
                            .add(egui::DragValue::new(&mut nw).range(10.0..=600.0).suffix(" pt"))
                            .changed()
                        {
                            *w = nw;
                            *h = nw * aspect;
                        }
                    }
                }

                if sel_kind == 'm' {
                    ui.separator();
                    ui.label(format!("{} selected", self.selected.len()));
                }

                if sel_kind != 'n' {
                    ui.separator();
                    if ui.button("Delete").clicked() {
                        self.delete_selected();
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(has_pdf, egui::Button::new("Save  Ctrl+S")).clicked() {
                        match self.save_pdf() {
                            Ok(out) => self.status = format!("Saved {}", out.display()),
                            Err(e) => self.status = format!("Save error: {e:#}"),
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pages_filter)
                            .hint_text("pages: 1-3,5  (empty=all)")
                            .desired_width(180.0),
                    )
                    .on_hover_text("Save only listed pages. Format: 1-3,5,7-9. Empty = all pages.");
                });
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let style = ui.style_mut();
                style.text_styles.insert(
                    egui::TextStyle::Body,
                    egui::FontId::new(15.0, egui::FontFamily::Proportional),
                );
                style.text_styles.insert(
                    egui::TextStyle::Button,
                    egui::FontId::new(15.0, egui::FontFamily::Proportional),
                );

                if !self.pages.is_empty() {
                    ui.label(format!("Page {}/{}", self.current + 1, self.pages.len()));
                    if ui.button("◀").clicked() && self.current > 0 {
                        self.current -= 1;
                        self.selected.clear();
                    }
                    if ui.button("▶").clicked() && self.current + 1 < self.pages.len() {
                        self.current += 1;
                        self.selected.clear();
                    }
                    ui.separator();
                }

                let is_error = self.status.to_lowercase().contains("error");
                let color = if is_error {
                    egui::Color32::from_rgb(220, 60, 60)
                } else {
                    ui.visuals().text_color()
                };
                let prefix = if is_error { "⚠  " } else { "" };
                ui.label(
                    egui::RichText::new(format!("{prefix}{}", self.status))
                        .color(color)
                        .size(if is_error { 16.0 } else { 15.0 })
                        .strong(),
                );
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.pages.is_empty() {
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
                return;
            }

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

            let cur = self.current;
            if self.pages[cur].texture.is_none() {
                let img = self.pages[cur].image.clone();
                let tex = ctx.load_texture(format!("page-{cur}"), img, egui::TextureOptions::LINEAR);
                self.pages[cur].texture = Some(tex);
            }

            self.draw_page(ui, ctx);
        });
    }
}

impl App {
    fn draw_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let cur = self.current;
        let page_pt = self.pages[cur].page_pt;
        let texture = self.pages[cur].texture.as_ref().unwrap().id();
        let avail = ui.available_size();
        let scale = (avail.x / page_pt.0).min(avail.y / page_pt.1).max(0.1);

        let display_size = egui::vec2(page_pt.0 * scale, page_pt.1 * scale);
        let (rect, response) = ui.allocate_exact_size(display_size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
        painter.image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        let overlay_rects: Vec<egui::Rect> = self.pages[cur]
            .overlays
            .iter()
            .map(|o| overlay_rect(o, rect, scale, ctx))
            .collect();

        for (i, overlay) in self.pages[cur].overlays.iter().enumerate() {
            let r = overlay_rects[i];
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
                    let target_baseline_y = r.min.y + INTER_ASCENT * size_pt * scale;
                    let draw_y = target_baseline_y - EGUI_BASELINE_FROM_TOP * size_pt * scale;
                    painter.text(
                        egui::pos2(r.min.x, draw_y),
                        egui::Align2::LEFT_TOP,
                        text,
                        egui::FontId::proportional(size_pt * scale),
                        egui::Color32::from_rgb(color[0], color[1], color[2]),
                    );
                }
            }

            if self.selected.contains(&i) {
                painter.rect_stroke(r.expand(2.0), 0.0, egui::Stroke::new(2.0, SEL_STROKE));
            }
        }

        // Rubber-band rect (drawn on top of overlays during selection drag)
        if let Some(start) = self.rubber_band {
            if let Some(end) = response.interact_pointer_pos().or(Some(start)) {
                let band = egui::Rect::from_two_pos(start, end);
                painter.rect(band, 0.0, RUBBER_FILL, egui::Stroke::new(1.5, RUBBER_STROKE));
            }
        }

        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let mut hit = None;
                for (i, r) in overlay_rects.iter().enumerate().rev() {
                    if r.contains(pos) {
                        hit = Some(i);
                        break;
                    }
                }
                if let Some(i) = hit {
                    if !self.selected.contains(&i) {
                        self.selected = vec![i];
                    }
                    self.drag_offsets = self
                        .selected
                        .iter()
                        .filter_map(|&j| overlay_rects.get(j).map(|r| (j, pos - r.left_top())))
                        .collect();
                    self.rubber_band = None;
                } else {
                    // Empty drag → start rubber-band; clear current selection.
                    self.selected.clear();
                    self.drag_offsets.clear();
                    self.rubber_band = Some(pos);
                }
            }
        }

        if response.dragged() && !self.drag_offsets.is_empty() {
            if let Some(pos) = response.interact_pointer_pos() {
                for &(idx, off) in &self.drag_offsets {
                    let new_top_left = pos - off;
                    let nx = (new_top_left.x - rect.min.x) / scale;
                    let ny = (new_top_left.y - rect.min.y) / scale;
                    if let Some(o) = self.pages[cur].overlays.get_mut(idx) {
                        match o {
                            Overlay::Image { x, y, .. } | Overlay::Text { x, y, .. } => {
                                *x = nx;
                                *y = ny;
                            }
                        }
                    }
                }
            }
        }

        if response.drag_stopped() {
            if let Some(start) = self.rubber_band.take() {
                let end = response.interact_pointer_pos().unwrap_or(start);
                let band = egui::Rect::from_two_pos(start, end);
                self.selected = overlay_rects
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| band.intersects(*r).then_some(i))
                    .collect();
            }
            self.drag_offsets.clear();
        }

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let mut hit = None;
                for (i, r) in overlay_rects.iter().enumerate().rev() {
                    if r.contains(pos) {
                        hit = Some(i);
                        break;
                    }
                }
                let ctrl = ctx.input(|i| i.modifiers.ctrl);
                match hit {
                    Some(i) if ctrl => {
                        if let Some(p) = self.selected.iter().position(|&x| x == i) {
                            self.selected.remove(p);
                        } else {
                            self.selected.push(i);
                        }
                    }
                    Some(i) => self.selected = vec![i],
                    None => self.selected.clear(),
                }
            }
        }
    }
}

fn color_controls(ui: &mut egui::Ui, color: &mut [u8; 3]) {
    let presets: [([u8; 3], &str); 3] =
        [([0, 0, 0], "Black"), ([200, 30, 30], "Red"), ([30, 60, 200], "Blue")];
    for (rgb, name) in presets {
        let selected = *color == rgb;
        let swatch = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
        ui.painter().rect_filled(rect, 3.0, swatch);
        let stroke = if selected {
            egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 160, 255))
        } else {
            egui::Stroke::new(1.0, egui::Color32::DARK_GRAY)
        };
        ui.painter().rect_stroke(if selected { rect.expand(1.0) } else { rect }, 3.0, stroke);
        if response.on_hover_text(name).clicked() {
            *color = rgb;
        }
    }
    let mut c32 = egui::Color32::from_rgb(color[0], color[1], color[2]);
    if egui::color_picker::color_edit_button_srgba(ui, &mut c32, egui::color_picker::Alpha::Opaque)
        .changed()
    {
        *color = [c32.r(), c32.g(), c32.b()];
    }
}

fn overlay_rect(o: &Overlay, page_rect: egui::Rect, scale: f32, ctx: &egui::Context) -> egui::Rect {
    match o {
        Overlay::Image { x, y, w, h, .. } => egui::Rect::from_min_size(
            page_rect.min + egui::vec2(x * scale, y * scale),
            egui::vec2(w * scale, h * scale),
        ),
        Overlay::Text { text, x, y, size_pt, .. } => {
            let fid = egui::FontId::proportional(size_pt * scale);
            let width = ctx.fonts(|f| f.layout_no_wrap(text.clone(), fid, egui::Color32::BLACK).size().x);
            let height = (INTER_ASCENT + INTER_DESCENT) * size_pt * scale;
            egui::Rect::from_min_size(
                page_rect.min + egui::vec2(x * scale, y * scale),
                egui::vec2(width.max(20.0), height),
            )
        }
    }
}

fn inter_face() -> &'static ttf_parser::Face<'static> {
    static FACE: OnceLock<ttf_parser::Face<'static>> = OnceLock::new();
    FACE.get_or_init(|| ttf_parser::Face::parse(INTER_TTF, 0).expect("parsing bundled Inter TTF"))
}

fn embed_inter_font(doc: &mut Document) -> Result<ObjectId> {
    let face = inter_face();
    let upem = face.units_per_em() as f32;
    let to_pdf = |v: i16| (v as f32 / upem * 1000.0).round() as i64;
    let bbox = face.global_bounding_box();

    let num_glyphs = face.number_of_glyphs();
    let mut widths_inner = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let advance = face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap_or(0);
        widths_inner.push(Object::Integer((advance as f32 / upem * 1000.0).round() as i64));
    }

    let mut ff2 = Stream::new(
        dictionary! { "Length1" => INTER_TTF.len() as i64 },
        INTER_TTF.to_vec(),
    );
    let _ = ff2.compress();
    let ff2_id = doc.add_object(ff2);

    let descriptor = dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Inter-Regular",
        "Flags" => 32_i64,
        "FontBBox" => Object::Array(vec![
            Object::Integer(to_pdf(bbox.x_min)),
            Object::Integer(to_pdf(bbox.y_min)),
            Object::Integer(to_pdf(bbox.x_max)),
            Object::Integer(to_pdf(bbox.y_max)),
        ]),
        "ItalicAngle" => 0_i64,
        "Ascent" => to_pdf(face.ascender()),
        "Descent" => to_pdf(face.descender()),
        "CapHeight" => face.capital_height().map(to_pdf).unwrap_or(728),
        "StemV" => 80_i64,
        "FontFile2" => Object::Reference(ff2_id),
    };
    let descriptor_id = doc.add_object(descriptor);

    let cid_font = dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "Inter-Regular",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::String(b"Adobe".to_vec(), lopdf::StringFormat::Literal),
            "Ordering" => Object::String(b"Identity".to_vec(), lopdf::StringFormat::Literal),
            "Supplement" => 0_i64,
        },
        "FontDescriptor" => Object::Reference(descriptor_id),
        "CIDToGIDMap" => "Identity",
        "DW" => 500_i64,
        "W" => Object::Array(vec![Object::Integer(0), Object::Array(widths_inner)]),
    };
    let cid_font_id = doc.add_object(cid_font);

    Ok(doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "Inter-Regular",
        "Encoding" => "Identity-H",
        "DescendantFonts" => Object::Array(vec![Object::Reference(cid_font_id)]),
    }))
}

fn encode_text_for_inter(text: &str) -> String {
    let face = inter_face();
    let mut out = String::with_capacity(text.len() * 4);
    for c in text.chars() {
        let gid = face.glyph_index(c).map(|g| g.0).unwrap_or(0);
        out.push_str(&format!("{gid:04X}"));
    }
    out
}

fn embed_image(doc: &mut Document, img: &DynamicImage) -> Result<ObjectId> {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();

    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let mut alpha = Vec::with_capacity((w * h) as usize);
    let mut has_alpha = false;
    for px in rgba.pixels() {
        rgb.extend_from_slice(&px.0[..3]);
        alpha.push(px.0[3]);
        if px.0[3] < 255 {
            has_alpha = true;
        }
    }

    let smask_id = if has_alpha {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&alpha)?;
        let smask = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => w as i64,
                "Height" => h as i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
                "Filter" => "FlateDecode",
            },
            enc.finish()?,
        );
        Some(doc.add_object(smask))
    } else {
        None
    };

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&rgb)?;
    let mut img_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => w as i64,
        "Height" => h as i64,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
        "Filter" => "FlateDecode",
    };
    if let Some(sid) = smask_id {
        img_dict.set("SMask", Object::Reference(sid));
    }
    Ok(doc.add_object(Stream::new(img_dict, enc.finish()?)))
}

fn wrap_and_append_overlay(doc: &mut Document, page_id: ObjectId, overlay_id: ObjectId) -> Result<()> {
    // Sandwich the original content in `q ... Q` so its CTM/state changes don't
    // leak into our overlay (some PDFs leave a non-identity CTM at end of stream).
    let q_id = doc.add_object(Stream::new(dictionary! {}, b"q\n".to_vec()));
    let original = {
        let p = doc.get_object(page_id)?.as_dict()?;
        p.get(b"Contents").ok().cloned()
    };
    let mut new_array: Vec<Object> = vec![Object::Reference(q_id)];
    match original {
        Some(Object::Reference(r)) => new_array.push(Object::Reference(r)),
        Some(Object::Array(arr)) => new_array.extend(arr),
        _ => {}
    }
    new_array.push(Object::Reference(overlay_id));
    doc.get_object_mut(page_id)?.as_dict_mut()?.set("Contents", Object::Array(new_array));
    Ok(())
}

fn add_page_resource(
    doc: &mut Document,
    page_id: ObjectId,
    res_type: &[u8],
    name: &[u8],
    obj_id: ObjectId,
) -> Result<()> {
    let current = {
        let page = doc.get_object(page_id)?.as_dict()?;
        match page.get(b"Resources") {
            Ok(Object::Reference(r)) => doc.get_object(*r)?.as_dict()?.clone(),
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => lopdf::Dictionary::new(),
        }
    };
    let mut resources = current;
    let mut sub = match resources.get(res_type) {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(r)) => doc.get_object(*r)?.as_dict()?.clone(),
        _ => lopdf::Dictionary::new(),
    };
    sub.set(name.to_vec(), Object::Reference(obj_id));
    resources.set(res_type.to_vec(), Object::Dictionary(sub));
    doc.get_object_mut(page_id)?.as_dict_mut()?.set("Resources", Object::Dictionary(resources));
    Ok(())
}
