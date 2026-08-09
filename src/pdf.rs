//! All PDF I/O: rendering source pages with pdfium, and writing overlays
//! back into the document via lopdf.

use anyhow::{Context, Result};
use eframe::egui;
use flate2::{write::ZlibEncoder, Compression};
use image::DynamicImage;
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use pdfium_render::prelude::*;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::overlay::Overlay;

// ----------------------------------------------------------------------------
// Bundled font + render parameters
// ----------------------------------------------------------------------------

pub(crate) const INTER_TTF: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");

const RENDER_WIDTH: u32 = 1800;
const RENDER_MAX_HEIGHT: u32 = 2400;

// ----------------------------------------------------------------------------
// Loading
// ----------------------------------------------------------------------------

/// One source page already rasterised to an `egui::ColorImage`.
pub struct LoadedPage {
    pub size_pt: (f32, f32),
    pub image: egui::ColorImage,
}

pub fn try_pdfium() -> Option<Pdfium> {
    Pdfium::bind_to_system_library()
        .or_else(|_| Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./")))
        .ok()
        .map(Pdfium::new)
}

pub fn load_pages(pdfium: &Pdfium, path: &Path) -> Result<Vec<LoadedPage>> {
    let doc = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("loading {}", path.display()))?;
    let cfg = PdfRenderConfig::new()
        .set_target_width(RENDER_WIDTH as i32)
        .set_maximum_height(RENDER_MAX_HEIGHT as i32);

    let mut pages = Vec::new();
    for page in doc.pages().iter() {
        let pw = page.width().value;
        let ph = page.height().value;
        let bitmap = page.render_with_config(&cfg).context("rendering page")?;
        let rgba = bitmap.as_image().to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
        pages.push(LoadedPage { size_pt: (pw, ph), image });
    }
    Ok(pages)
}

// ----------------------------------------------------------------------------
// Text detection
// ----------------------------------------------------------------------------

/// One bounding box per **word** of text found inside `rect`, in displayed
/// top-down PDF-point coordinates. A word is a run of consecutive
/// non-whitespace glyphs on the same baseline in document order. Returned
/// rects are clipped to `rect` so OCR-inflated char cells don't paint bars
/// far beyond the user's drag. Empty return means the page has no
/// extractable text (e.g. a scan without OCR) or the drag missed all text.
pub fn detect_text_lines(
    pdfium: &Pdfium,
    path: &Path,
    page_idx: usize,
    rect: (f32, f32, f32, f32),
) -> Vec<(f32, f32, f32, f32)> {
    let Ok(doc) = pdfium.load_pdf_from_file(path, None) else { return vec![] };
    let pages = doc.pages();
    let Ok(page) = pages.get(page_idx as u16) else { return vec![] };
    let Ok(text) = page.text() else { return vec![] };
    let frame = page_frame(&page);
    let mut out = vec![];
    for_each_word(&text, &frame, |word_disp| {
        if let Some(hit) = match_and_clip(word_disp, rect) {
            out.push(hit);
        }
    });
    out
}

/// Page dimensions + intrinsic rotation, cached in a single struct so the
/// caller doesn't juggle a 3-tuple.
struct PageFrame {
    /// pdfium's `/Rotate` value (0/90/180/270). Anything else falls back to 0.
    rotate: i32,
    /// Unrotated MediaBox width in points.
    mb_w: f32,
    /// Unrotated MediaBox height in points.
    mb_h: f32,
}

fn page_frame(page: &PdfPage) -> PageFrame {
    // pdfium's `width()`/`height()` are the *displayed* dims (they already
    // apply `/Rotate`). We reconstruct the unrotated MediaBox by swapping
    // when the page is on its side.
    let rot_w = page.width().value;
    let rot_h = page.height().value;
    let rotate = page.rotation().map(rotation_degrees).unwrap_or(0);
    let (mb_w, mb_h) = if rotate == 90 || rotate == 270 {
        (rot_h, rot_w)
    } else {
        (rot_w, rot_h)
    };
    PageFrame { rotate, mb_w, mb_h }
}

/// Convert pdfium-render's `PdfPageRenderRotation` variant into an integer
/// degrees value (0/90/180/270).
fn rotation_degrees(rot: pdfium_render::prelude::PdfPageRenderRotation) -> i32 {
    use pdfium_render::prelude::PdfPageRenderRotation as R;
    match rot {
        R::None => 0,
        R::Degrees90 => 90,
        R::Degrees180 => 180,
        R::Degrees270 => 270,
    }
}

/// Walk `text` in document order and emit one word rect per maximal run of
/// consecutive non-whitespace glyphs on the same visual line. Emitted rects
/// are `(x, y, w, h)` in the *displayed* top-down frame (i.e. what the user
/// sees on-screen), so the baseline check works on rotated pages too — a
/// `/Rotate 90` page's chars share a common displayed-y even though their
/// MediaBox y-up positions vary along y.
///
/// pdfium exposes `text.segments()` which sounds like it does this, but its
/// bounds include the phantom advance to the *next* segment — on
/// column-aligned layouts (invoices, tables) that spans half a line.
fn for_each_word(
    text: &PdfPageText,
    frame: &PageFrame,
    mut emit: impl FnMut((f32, f32, f32, f32)),
) {
    // `current` accumulates the (left, top, right, bottom) bbox of the word
    // being built, in displayed top-down coords.
    let mut current: Option<(f32, f32, f32, f32)> = None;
    for c in text.chars().iter() {
        if c.unicode_char().is_some_and(|ch| ch.is_whitespace()) {
            if let Some((l, t, r, b)) = current.take() {
                emit((l, t, r - l, b - t));
            }
            continue;
        }
        let Ok(bounds) = c.tight_bounds() else { continue };
        let (mb_l, mb_b) = (bounds.left().value, bounds.bottom().value);
        let (mb_w_c, mb_h_c) = (bounds.width().value, bounds.height().value);
        if mb_w_c <= 0.0 || mb_h_c <= 0.0 {
            continue;
        }
        let (dx, dy, dw, dh) =
            media_box_to_displayed(frame.rotate, frame.mb_w, frame.mb_h, mb_l, mb_b, mb_w_c, mb_h_c);
        let (l, t, r, b) = (dx, dy, dx + dw, dy + dh);
        if let Some((_, prev_t, _, prev_b)) = current
            && !on_same_baseline(prev_t, prev_b, t, b)
            && let Some((cl, ct, cr, cb)) = current.take()
        {
            emit((cl, ct, cr - cl, cb - ct));
        }
        current = Some(match current {
            None => (l, t, r, b),
            Some((cl, ct, cr, cb)) => (cl.min(l), ct.min(t), cr.max(r), cb.max(b)),
        });
    }
    if let Some((l, t, r, b)) = current {
        emit((l, t, r - l, b - t));
    }
}

/// True if the new glyph (displayed-y range `[t, b]`) sits on the same
/// baseline as the current word (previous glyph `[prev_t, prev_b]`).
/// Threshold is half the glyph height so multi-line jumps break the word
/// even when the intervening whitespace char is missing.
fn on_same_baseline(prev_t: f32, prev_b: f32, t: f32, b: f32) -> bool {
    let half_h = (prev_b - prev_t) * 0.5;
    (t - prev_t).abs() <= half_h && (b - prev_b).abs() <= half_h
}

/// Decide whether a word rect should be marked given the user's selection,
/// and if so return the clipped-to-selection version. Both inputs are in
/// the same (displayed top-down) frame.
///
/// A word qualifies when either the word's center lies inside the
/// selection *or* the selection's center lies inside the word — this
/// handles both wide drags (word center in sel) and tight drags on a
/// small part of an OCR-inflated word rect (sel center in word).
/// The returned rect is the intersection, so an OCR-inflated word never
/// paints outside what the user actually dragged.
fn match_and_clip(
    word: (f32, f32, f32, f32),
    sel: (f32, f32, f32, f32),
) -> Option<(f32, f32, f32, f32)> {
    let (wl, wt, ww, wh) = word;
    let (sl, st, sw, sh) = sel;
    let (wr, wb) = (wl + ww, wt + wh);
    let (sr, sb) = (sl + sw, st + sh);
    let word_mid = (wl + ww * 0.5, wt + wh * 0.5);
    let sel_mid = (sl + sw * 0.5, st + sh * 0.5);
    let in_rect = |px: f32, py: f32, l: f32, t: f32, r: f32, b: f32| {
        px >= l && px <= r && py >= t && py <= b
    };
    let hit = in_rect(word_mid.0, word_mid.1, sl, st, sr, sb)
        || in_rect(sel_mid.0, sel_mid.1, wl, wt, wr, wb);
    if !hit {
        return None;
    }
    let cl = wl.max(sl);
    let ct = wt.max(st);
    let cr = wr.min(sr);
    let cb = wb.min(sb);
    (cr > cl && cb > ct).then_some((cl, ct, cr - cl, cb - ct))
}

/// Inverse of [`rect_to_media_box`]: takes a rectangle expressed in the
/// *unrotated* MediaBox y-up frame (as pdfium reports for text bounds) and
/// returns its equivalent in the *rotated display* top-down frame that the
/// user sees on-screen. `mb_w`/`mb_h` are the unrotated MediaBox dims;
/// `rl`/`rb`/`rw`/`rh` are the rectangle's left/bottom/width/height.
fn media_box_to_displayed(
    rotate: i32,
    mb_w: f32,
    mb_h: f32,
    rl: f32,
    rb: f32,
    rw: f32,
    rh: f32,
) -> (f32, f32, f32, f32) {
    match rotate.rem_euclid(360) {
        90 => (rb, rl, rh, rw),
        180 => (mb_w - rl - rw, rb, rw, rh),
        270 => (mb_h - rb - rh, mb_w - rl - rw, rh, rw),
        _ => (rl, mb_h - rb - rh, rw, rh),
    }
}

// ----------------------------------------------------------------------------
// Saving
// ----------------------------------------------------------------------------

/// Write `<input>_signed.pdf` (or `_masked.pdf` if any overlay is a redaction,
/// with `_N` appended when the target already exists) with `overlays` applied
/// to each page. `pages_filter` like "1-3,5" keeps only those pages in the
/// output; empty string keeps all.
pub fn save(
    pdfium: Option<&Pdfium>,
    input: &Path,
    overlays_per_page: &[Vec<Overlay>],
    page_size_pt: &[(f32, f32)],
    pages_filter: &str,
) -> Result<PathBuf> {
    let has_redact = overlays_per_page.iter().flatten().any(Overlay::is_redact);
    let output = unique_output_path(input, has_redact);

    let mut doc = load_pdf_robust(input, pdfium)?;
    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let total = page_ids.len();
    let keep = parse_keep_filter(pages_filter, total)?;

    let mut inter_font_id: Option<ObjectId> = None;
    for (idx, &page_id) in page_ids.iter().enumerate() {
        if let Some(k) = &keep
            && !k.contains(&(idx + 1)) {
                continue;
            }
        let overlays = match overlays_per_page.get(idx) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };
        let rotated_size = *page_size_pt.get(idx).unwrap_or(&(595.0, 842.0));
        write_overlays_for_page(&mut doc, page_id, rotated_size, overlays, &mut inter_font_id)?;
    }

    if let Some(k) = &keep {
        let drop: Vec<u32> = (1..=total as u32)
            .filter(|p| !k.contains(&(*p as usize)))
            .collect();
        if !drop.is_empty() {
            doc.delete_pages(&drop);
        }
    }

    doc.save(&output)?;
    Ok(output)
}

fn unique_output_path(input: &Path, has_redact: bool) -> PathBuf {
    let raw_stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "out".into());
    // Recover the original base name: if the input is itself a previous
    // output (`foo_signed`, `foo_masked_3`, …), strip that tail so
    // re-saving `foo_masked.pdf` produces `foo_masked_1.pdf` rather than
    // `foo_masked_masked.pdf`.
    let stem = strip_output_suffix(&raw_stem);
    let parent = input.parent().unwrap_or(Path::new("."));
    let suffix = if has_redact { "masked" } else { "signed" };
    let mut output = parent.join(format!("{stem}_{suffix}.pdf"));
    let mut n = 1;
    while output.exists() {
        output = parent.join(format!("{stem}_{suffix}_{n}.pdf"));
        n += 1;
    }
    output
}

/// Read the effective `/Rotate` value for a page. `/Rotate` is inheritable
/// through the `/Parent` chain, and the spec constrains it to a multiple of
/// 90; anything else (or missing) is treated as 0.
fn read_page_rotate(doc: &Document, page_id: ObjectId) -> i32 {
    let mut cur = page_id;
    for _ in 0..16 {
        let Ok(obj) = doc.get_object(cur) else { return 0 };
        let Ok(dict) = obj.as_dict() else { return 0 };
        if let Ok(v) = dict.get(b"Rotate")
            && let Ok(n) = v.as_i64() {
            let r = (n.rem_euclid(360)) as i32;
            return match r {
                0 | 90 | 180 | 270 => r,
                _ => 0,
            };
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(r)) => cur = *r,
            _ => return 0,
        }
    }
    0
}

/// Read the effective `/MediaBox` for a page as `(x0, y0)` — the lower-left
/// corner of the visible area in the PDF user-space coordinate system.
/// Also inheritable through the `/Parent` chain. Returns `(0.0, 0.0)` if
/// missing or malformed; that's the common case (~99% of PDFs).
///
/// A non-zero origin (e.g. `[0 7.83 595.5 850.08]`, common from some Canva
/// exports) means the visible bottom-left sits at `(x0, y0)` in PDF space,
/// not at `(0, 0)`. We must offset every emitted coordinate by this or the
/// entire overlay layer drifts by that amount.
fn read_page_media_box_origin(doc: &Document, page_id: ObjectId) -> (f32, f32) {
    let mut cur = page_id;
    for _ in 0..16 {
        let Ok(obj) = doc.get_object(cur) else { return (0.0, 0.0) };
        let Ok(dict) = obj.as_dict() else { return (0.0, 0.0) };
        if let Ok(mb) = dict.get(b"MediaBox")
            && let Ok(arr) = mb.as_array()
            && arr.len() >= 2
        {
            let to_f32 = |o: &Object| -> f32 {
                o.as_f32()
                    .or_else(|_| o.as_i64().map(|n| n as f32))
                    .unwrap_or(0.0)
            };
            return (to_f32(&arr[0]), to_f32(&arr[1]));
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(r)) => cur = *r,
            _ => return (0.0, 0.0),
        }
    }
    (0.0, 0.0)
}

/// Transform a top-down `(x, y, w, h)` rectangle expressed in the *rotated
/// display* frame into the `(px, py, pw, ph)` tuple to feed into a PDF
/// `re` operator — i.e. bottom-left corner in the *unrotated* MediaBox
/// y-up frame plus width/height. `mb_w`/`mb_h` are the unrotated MediaBox
/// dimensions.
fn rect_to_media_box(
    rotate: i32,
    mb_w: f32,
    mb_h: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> (f32, f32, f32, f32) {
    match rotate.rem_euclid(360) {
        90 => (y, x, h, w),
        180 => (mb_w - x - w, y, w, h),
        270 => (mb_w - y - h, mb_h - x - w, h, w),
        _ => (x, mb_h - y - h, w, h),
    }
}

/// Strip a trailing `_signed` / `_masked` (optionally followed by `_<N>`
/// counter) from `stem`. Everything else is returned unchanged, so a
/// user file that just happens to contain the substring elsewhere is
/// left alone.
fn strip_output_suffix(stem: &str) -> &str {
    for suf in ["_signed", "_masked"] {
        if let Some(rest) = stem.strip_suffix(suf) {
            return rest;
        }
    }
    if let Some(pos) = stem.rfind('_') {
        let tail = &stem[pos + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            let head = &stem[..pos];
            for suf in ["_signed", "_masked"] {
                if let Some(rest) = head.strip_suffix(suf) {
                    return rest;
                }
            }
        }
    }
    stem
}

fn parse_keep_filter(filter: &str, total: usize) -> Result<Option<HashSet<usize>>> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed: HashSet<usize> = parse_page_range(trimmed, total).into_iter().collect();
    if parsed.is_empty() {
        anyhow::bail!("Pages filter '{trimmed}' selects no pages");
    }
    Ok(Some(parsed))
}

fn write_overlays_for_page(
    doc: &mut Document,
    page_id: ObjectId,
    rotated_size_pt: (f32, f32),
    overlays: &[Overlay],
    inter_font_id: &mut Option<ObjectId>,
) -> Result<()> {
    // Pick resource names that don't collide with the source PDF — overwriting
    // an existing /F1 or /Im1 corrupts the original glyph mapping.
    let existing_fonts = page_resource_keys(doc, page_id, b"Font");
    let mut used_xobj = page_resource_keys(doc, page_id, b"XObject");
    let font_name = pick_free_name(&existing_fonts, "F");

    // Overlay coordinates come from the user in the *rotated display* frame —
    // pdfium renders the page with `/Rotate` applied, so what the user sees
    // (and marks) is already-rotated. Text/image emission still lives in the
    // rotated frame (pre-rotation orientation isn't handled — see below), but
    // redact rects transform cleanly and we map them into the unrotated
    // MediaBox frame before writing.
    let rotate = read_page_rotate(doc, page_id);
    let (mb_x0, mb_y0) = read_page_media_box_origin(doc, page_id);
    let (rot_w, rot_h) = rotated_size_pt;
    let (mb_w, mb_h) = if rotate == 90 || rotate == 270 {
        (rot_h, rot_w)
    } else {
        (rot_w, rot_h)
    };
    // `page_h_pt` is the rotated-frame height, used by Text/Image emission
    // (which stays in the rotated frame — orientation not yet handled).
    let page_h_pt = rot_h;

    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(b"Q\n"); // close `q` prepended by wrap_and_append_overlay

    let mut images: Vec<(Vec<u8>, ObjectId)> = vec![];
    let mut text_used = false;

    for overlay in overlays {
        match overlay {
            Overlay::Image { path, x, y, w, h } => {
                let img = image::open(path)
                    .with_context(|| format!("opening image {}", path.display()))?;
                let img_id = embed_image(doc, &img)?;
                let res_name = pick_free_name(&used_xobj, "Im");
                used_xobj.push(res_name.as_bytes().to_vec());
                let pdf_x = mb_x0 + x;
                let pdf_y = mb_y0 + page_h_pt - y - h;
                content.extend_from_slice(
                    format!(
                        "q\n{:.4} 0 0 {:.4} {:.4} {:.4} cm\n/{} Do\nQ\n",
                        w, h, pdf_x, pdf_y, res_name
                    )
                    .as_bytes(),
                );
                images.push((res_name.into_bytes(), img_id));
            }
            Overlay::Text { text, x, y, size_pt, color } => {
                text_used = true;
                // First line's baseline; further lines advance by the leading
                // (`TL` + `T*`) so multi-line text wraps exactly as the egui
                // preview lays it out.
                let baseline = y + size_pt * crate::theme::INTER_BASELINE_RATIO;
                let pdf_x = mb_x0 + x;
                let pdf_y = mb_y0 + page_h_pt - baseline;
                let leading = size_pt * crate::theme::INTER_LINE_HEIGHT_RATIO;
                let r = color[0] as f32 / 255.0;
                let g = color[1] as f32 / 255.0;
                let b = color[2] as f32 / 255.0;
                content.extend_from_slice(
                    format!(
                        "BT\n/{} {:.4} Tf\n{:.4} TL\n{:.4} {:.4} {:.4} rg\n{:.4} {:.4} Td\n",
                        font_name, size_pt, leading, r, g, b, pdf_x, pdf_y
                    )
                    .as_bytes(),
                );
                for (li, line) in text.split('\n').enumerate() {
                    if li > 0 {
                        content.extend_from_slice(b"T*\n");
                    }
                    content.extend_from_slice(b"<");
                    content.extend_from_slice(encode_text_for_inter(line).as_bytes());
                    content.extend_from_slice(b"> Tj\n");
                }
                content.extend_from_slice(b"ET\n");
            }
            Overlay::Redact { x, y, w, h } => {
                // Rotated-display (top-down) → MediaBox y-up rectangle so
                // `/Rotate 90/180/270` pages get the redaction bar at the
                // same visual position the user marked. Add the MediaBox
                // origin so pages with a non-zero MediaBox lower-left
                // (e.g. `[0 7.83 W H]`) don't shift the bar.
                let (px, py, pw, ph) = rect_to_media_box(rotate, mb_w, mb_h, *x, *y, *w, *h);
                let (px, py) = (px + mb_x0, py + mb_y0);
                content.extend_from_slice(
                    format!(
                        "q\n0 0 0 rg\n{px:.4} {py:.4} {pw:.4} {ph:.4} re\nf\nQ\n",
                    )
                    .as_bytes(),
                );
            }
            Overlay::PendingMark { .. } => {
                // UI-only state; not persisted in the saved PDF. Should be
                // committed to `Redact` (via `b`) or discarded (Esc) first.
            }
        }
    }

    let mut stream = Stream::new(dictionary! {}, content);
    let _ = stream.compress();
    let overlay_id = doc.add_object(stream);
    wrap_and_append_overlay(doc, page_id, overlay_id)?;

    if text_used {
        let font_id = match inter_font_id {
            Some(id) => *id,
            None => {
                let id = embed_inter_font(doc)?;
                *inter_font_id = Some(id);
                id
            }
        };
        add_page_resource(doc, page_id, b"Font", font_name.as_bytes(), font_id)?;
    }
    for (name, img_id) in &images {
        add_page_resource(doc, page_id, b"XObject", name, *img_id)?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// PDF document loading (tolerant)
// ----------------------------------------------------------------------------

/// First try lopdf strict; on failure round-trip through pdfium to normalise
/// the structure (decompress xref-streams, unpack ObjStm) and try again.
/// Handles Chrome/Skia output, compressed xref tables, and incrementally-
/// updated PDFs that lopdf can't parse.
fn load_pdf_robust(path: &Path, pdfium: Option<&Pdfium>) -> Result<Document> {
    match Document::load(path) {
        Ok(d) => return Ok(d),
        Err(e) => eprintln!("[load] strict load failed ({e}), normalising via pdfium"),
    }
    let pdfium = pdfium.context("pdfium not loaded — cannot normalise broken PDF")?;
    let path_str = path.to_str().context("non-UTF8 PDF path")?;
    let doc = pdfium
        .load_pdf_from_file(path_str, None)
        .context("pdfium failed to load PDF")?;
    let bytes = doc.save_to_bytes().context("pdfium failed to serialize PDF")?;
    Document::load_mem(&bytes).context("loading PDF after pdfium normalisation")
}

// ----------------------------------------------------------------------------
// Page-range parsing
// ----------------------------------------------------------------------------

fn parse_page_range(s: &str, total: usize) -> Vec<usize> {
    let mut out: Vec<usize> = vec![];
    let push_unique = |out: &mut Vec<usize>, p: usize| {
        if (1..=total).contains(&p) && !out.contains(&p) {
            out.push(p);
        }
    };
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                for p in lo..=hi {
                    push_unique(&mut out, p);
                }
            }
        } else if let Ok(p) = part.parse::<usize>() {
            push_unique(&mut out, p);
        }
    }
    out
}

// ----------------------------------------------------------------------------
// Inter font embedding (Type0 / Identity-H, full embed)
// ----------------------------------------------------------------------------

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

/// Encode a UTF-8 string as the hex glyph-id stream Identity-H expects.
fn encode_text_for_inter(text: &str) -> String {
    let face = inter_face();
    let mut out = String::with_capacity(text.len() * 4);
    for c in text.chars() {
        let gid = face.glyph_index(c).map(|g| g.0).unwrap_or(0);
        out.push_str(&format!("{gid:04X}"));
    }
    out
}

// ----------------------------------------------------------------------------
// Image embedding (RGB image XObject, optional alpha SMask)
// ----------------------------------------------------------------------------

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

// ----------------------------------------------------------------------------
// lopdf primitives
// ----------------------------------------------------------------------------

fn wrap_and_append_overlay(doc: &mut Document, page_id: ObjectId, overlay_id: ObjectId) -> Result<()> {
    // Sandwich the original content in `q ... Q` so its CTM/state changes
    // don't leak into our overlay (some PDFs leave a non-identity CTM at
    // end of stream).
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
    doc.get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Contents", Object::Array(new_array));
    Ok(())
}

/// Keys present in `<page>.Resources.<res_type>` — used to avoid overwriting
/// existing /F1, /Im1, etc. defined by the source PDF.
fn page_resource_keys(doc: &Document, page_id: ObjectId, res_type: &[u8]) -> Vec<Vec<u8>> {
    let res_obj = {
        let Ok(page_obj) = doc.get_object(page_id) else { return vec![] };
        let Ok(page) = page_obj.as_dict() else { return vec![] };
        match page.get(b"Resources").ok() {
            Some(o) => o.clone(),
            None => return vec![],
        }
    };
    let resources = match res_obj {
        Object::Reference(r) => match doc.get_object(r).and_then(|o| o.as_dict()) {
            Ok(d) => d.clone(),
            Err(_) => return vec![],
        },
        Object::Dictionary(d) => d,
        _ => return vec![],
    };
    let sub_obj = match resources.get(res_type).ok() {
        Some(o) => o.clone(),
        None => return vec![],
    };
    let sub = match sub_obj {
        Object::Reference(r) => match doc.get_object(r).and_then(|o| o.as_dict()) {
            Ok(d) => d.clone(),
            Err(_) => return vec![],
        },
        Object::Dictionary(d) => d,
        _ => return vec![],
    };
    sub.iter().map(|(k, _)| k.clone()).collect()
}

fn pick_free_name(taken: &[Vec<u8>], prefix: &str) -> String {
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|name| !taken.iter().any(|k| k.as_slice() == name.as_bytes()))
        .expect("infinite generator yields a free name")
}

fn add_page_resource(
    doc: &mut Document,
    page_id: ObjectId,
    res_type: &[u8],
    name: &[u8],
    obj_id: ObjectId,
) -> Result<()> {
    let mut resources = {
        let page = doc.get_object(page_id)?.as_dict()?;
        match page.get(b"Resources") {
            Ok(Object::Reference(r)) => doc.get_object(*r)?.as_dict()?.clone(),
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => lopdf::Dictionary::new(),
        }
    };
    let mut sub = match resources.get(res_type) {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(r)) => doc.get_object(*r)?.as_dict()?.clone(),
        _ => lopdf::Dictionary::new(),
    };
    sub.set(name.to_vec(), Object::Reference(obj_id));
    resources.set(res_type.to_vec(), Object::Dictionary(sub));
    doc.get_object_mut(page_id)?
        .as_dict_mut()?
        .set("Resources", Object::Dictionary(resources));
    Ok(())
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_and_clip_hits_when_centers_overlap() {
        // Word fits inside selection → returned rect equals the word.
        let word = (10.0f32, 10.0, 20.0, 8.0);
        let sel = (0.0f32, 0.0, 100.0, 50.0);
        assert_eq!(match_and_clip(word, sel), Some(word));

        // Selection fully inside word (tight drag on OCR-inflated word) →
        // clipped to selection.
        let word = (0.0f32, 0.0, 100.0, 20.0);
        let sel = (30.0f32, 5.0, 10.0, 10.0);
        assert_eq!(match_and_clip(word, sel), Some(sel));

        // Word center in selection, but word extends beyond → clipped to
        // intersection.
        let word = (50.0f32, 10.0, 60.0, 10.0);
        let sel = (60.0f32, 5.0, 40.0, 30.0);
        // intersection: x=[60, 100] w=40, y=[10, 20] h=10
        assert_eq!(match_and_clip(word, sel), Some((60.0, 10.0, 40.0, 10.0)));

        // Neither center in the other → no hit.
        let word = (0.0f32, 0.0, 10.0, 10.0);
        let sel = (50.0f32, 50.0, 10.0, 10.0);
        assert_eq!(match_and_clip(word, sel), None);

        // Touching but not overlapping → no hit (clip yields degenerate).
        let word = (0.0f32, 0.0, 10.0, 10.0);
        let sel = (10.0f32, 0.0, 10.0, 10.0);
        assert_eq!(match_and_clip(word, sel), None);
    }

    #[test]
    fn media_box_to_displayed_is_inverse_of_rect_to_media_box() {
        // Round-trip: displayed → MediaBox → displayed must be identity for
        // every rotation. `rect_to_media_box` takes displayed (top-down) →
        // MediaBox y-up bottom-left+dims; `media_box_to_displayed` takes
        // MediaBox y-up bottom-left+dims → displayed (top-down).
        for &rotate in &[0, 90, 180, 270] {
            let (mb_w, mb_h) = if rotate == 90 || rotate == 270 {
                (200.0f32, 100.0f32)
            } else {
                (100.0f32, 200.0f32)
            };
            let disp = (13.0f32, 27.0f32, 41.0f32, 19.0f32);
            let (px, py, pw, ph) =
                rect_to_media_box(rotate, mb_w, mb_h, disp.0, disp.1, disp.2, disp.3);
            let back = media_box_to_displayed(rotate, mb_w, mb_h, px, py, pw, ph);
            assert!(
                (back.0 - disp.0).abs() < 1e-3
                    && (back.1 - disp.1).abs() < 1e-3
                    && (back.2 - disp.2).abs() < 1e-3
                    && (back.3 - disp.3).abs() < 1e-3,
                "rotate={rotate}: round-trip mismatch got {back:?}, expected {disp:?}"
            );
        }
    }

    #[test]
    fn rect_to_media_box_handles_all_rotations() {
        // Unrotated 100x200 page (mb_w=100, mb_h=200). Rotated view
        // dimensions differ per rotate but that's the caller's concern —
        // this fn takes MediaBox dims directly.
        // Redact at rotated-display (x=10, y=20, w=30, h=40).
        assert_eq!(rect_to_media_box(0, 100.0, 200.0, 10.0, 20.0, 30.0, 40.0), (10.0, 140.0, 30.0, 40.0));
        // /Rotate 90: swap axes, y_rd → x_mb, x_rd → y_mb.
        assert_eq!(rect_to_media_box(90, 200.0, 100.0, 10.0, 20.0, 30.0, 40.0), (20.0, 10.0, 40.0, 30.0));
        // /Rotate 180: flip x, keep y (in top-down / y-up terms this
        // ends up as `(mb_w - x - w, y, w, h)`).
        assert_eq!(rect_to_media_box(180, 100.0, 200.0, 10.0, 20.0, 30.0, 40.0), (60.0, 20.0, 30.0, 40.0));
        // /Rotate 270: same axis swap as 90 but with both flips.
        assert_eq!(rect_to_media_box(270, 200.0, 100.0, 10.0, 20.0, 30.0, 40.0), (140.0, 60.0, 40.0, 30.0));
        // Any other rotate value falls back to /Rotate 0.
        assert_eq!(rect_to_media_box(45, 100.0, 200.0, 10.0, 20.0, 30.0, 40.0), (10.0, 140.0, 30.0, 40.0));
    }

    #[test]
    fn strip_output_suffix_recovers_base_name() {
        assert_eq!(strip_output_suffix("contract"), "contract");
        assert_eq!(strip_output_suffix("contract_signed"), "contract");
        assert_eq!(strip_output_suffix("contract_masked"), "contract");
        assert_eq!(strip_output_suffix("contract_signed_1"), "contract");
        assert_eq!(strip_output_suffix("contract_masked_42"), "contract");
        // Only a trailing counter with no known suffix under it → unchanged.
        assert_eq!(strip_output_suffix("draft_v2"), "draft_v2");
        assert_eq!(strip_output_suffix("report_2024"), "report_2024");
        // Suffix-lookalike embedded elsewhere → unchanged.
        assert_eq!(strip_output_suffix("masked_contract"), "masked_contract");
        assert_eq!(strip_output_suffix("signed_by_alice"), "signed_by_alice");
    }

    /// Write a minimal blank A4 page via lopdf so the tests are self-contained
    /// (no external `gs`/fixture needed — only pdfium for rendering).
    fn make_blank_a4(path: &Path) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).unwrap();
    }

    /// End-to-end baseline check: stamp all-caps text (no descenders) at a
    /// known top-edge position onto a blank A4, render the saved PDF, and read
    /// the bottom of the dark pixels — that bottom edge is the glyph baseline,
    /// and it must equal `y + size * INTER_BASELINE_RATIO` (the same formula
    /// the preview uses to draw the egui galley). Skips if pdfium is missing.
    #[test]
    fn saved_text_lands_on_preview_baseline() {
        let Some(pdfium) = try_pdfium() else {
            eprintln!("skip: pdfium not available");
            return;
        };
        let blank = Path::new("target/testdata/blank_baseline.pdf");
        make_blank_a4(blank);

        const X: f32 = 200.0;
        const Y_TOP: f32 = 100.0;
        const SIZE: f32 = 60.0;
        let page = load_pages(&pdfium, blank).unwrap().remove(0);
        let (pw, ph) = page.size_pt;

        let overlays = vec![vec![Overlay::Text {
            text: "BASELINE".to_owned(),
            x: X,
            y: Y_TOP,
            size_pt: SIZE,
            color: [0, 0, 0],
        }]];
        let out = save(Some(&pdfium), blank, &overlays, &[(pw, ph)], "").unwrap();

        // Render the saved page at ~1px per point.
        let doc = pdfium.load_pdf_from_file(out.to_str().unwrap(), None).unwrap();
        let cfg = PdfRenderConfig::new().set_target_width(pw as i32);
        let pages = doc.pages();
        let page0 = pages.iter().next().unwrap();
        let bmp = page0.render_with_config(&cfg).unwrap();
        let img = bmp.as_image().to_luma8();
        let (iw, ih) = (img.width() as f32, img.height() as f32);

        // Bounding box of dark (text) pixels.
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (f32::MAX, 0.0_f32, f32::MAX, 0.0_f32);
        for (px, py, p) in img.enumerate_pixels() {
            if p.0[0] < 128 {
                min_x = min_x.min(px as f32);
                max_x = max_x.max(px as f32);
                min_y = min_y.min(py as f32);
                max_y = max_y.max(py as f32);
            }
        }
        assert!(max_x > 0.0, "no text rendered in saved PDF");

        // Pixel → point (top-down).
        let to_pt_x = |px: f32| px * pw / iw;
        let to_pt_y = |py: f32| py * ph / ih;
        let left_pt = to_pt_x(min_x);
        let cap_top_pt = to_pt_y(min_y);
        let baseline_pt = to_pt_y(max_y);
        let expected_baseline = Y_TOP + SIZE * crate::theme::INTER_BASELINE_RATIO;

        eprintln!(
            "left={left_pt:.1} (exp ~{X}), cap_top={cap_top_pt:.1}, baseline={baseline_pt:.1} (exp ~{expected_baseline:.1})"
        );
        // Left edge of the glyphs sits at the overlay x (Inter has a small left
        // side bearing, so allow a few points of slack).
        assert!((left_pt - X).abs() < 8.0, "left edge {left_pt:.1} != {X}");
        // Baseline (bottom of caps) matches the preview formula within a couple
        // of points (rasterisation + antialias fringe).
        assert!(
            (baseline_pt - expected_baseline).abs() < 3.0,
            "baseline {baseline_pt:.1} != expected {expected_baseline:.1}"
        );

        let _ = std::fs::remove_file(&out);
    }

    /// Multi-line overlay text must wrap in the saved PDF (it used to collapse
    /// onto one line because `\n` encoded as the notdef glyph). Stamp two
    /// lines, render, and confirm two distinct dark bands spaced one line-
    /// height apart.
    #[test]
    fn saved_multiline_text_wraps() {
        let Some(pdfium) = try_pdfium() else { return };
        let blank = Path::new("target/testdata/blank_multiline.pdf");
        make_blank_a4(blank);

        const SIZE: f32 = 60.0;
        let page = load_pages(&pdfium, blank).unwrap().remove(0);
        let (pw, ph) = page.size_pt;
        let overlays = vec![vec![Overlay::Text {
            text: "HH\nHH".to_owned(),
            x: 150.0,
            y: 100.0,
            size_pt: SIZE,
            color: [0, 0, 0],
        }]];
        let out = save(Some(&pdfium), blank, &overlays, &[(pw, ph)], "").unwrap();

        let doc = pdfium.load_pdf_from_file(out.to_str().unwrap(), None).unwrap();
        let cfg = PdfRenderConfig::new().set_target_width(pw as i32);
        let pages = doc.pages();
        let page0 = pages.iter().next().unwrap();
        let bmp = page0.render_with_config(&cfg).unwrap();
        let img = bmp.as_image().to_luma8();
        let (iw, ih) = (img.width(), img.height());

        // Group rows that contain dark pixels into contiguous vertical bands.
        let mut bands: Vec<(u32, u32)> = vec![];
        let mut in_band = false;
        for y in 0..ih {
            let dark = (0..iw).any(|x| img.get_pixel(x, y).0[0] < 128);
            match (dark, in_band) {
                (true, false) => {
                    bands.push((y, y));
                    in_band = true;
                }
                (true, true) => bands.last_mut().unwrap().1 = y,
                (false, _) => in_band = false,
            }
        }

        assert_eq!(bands.len(), 2, "expected 2 text lines, got bands {bands:?}");
        // Band centres should be ~one line-height (SIZE * ratio) apart.
        let to_pt_y = |py: u32| py as f32 * ph / ih as f32;
        let c0 = to_pt_y((bands[0].0 + bands[0].1) / 2);
        let c1 = to_pt_y((bands[1].0 + bands[1].1) / 2);
        let gap = c1 - c0;
        let expected = SIZE * crate::theme::INTER_LINE_HEIGHT_RATIO;
        eprintln!("line gap = {gap:.1} pt (expected ~{expected:.1})");
        assert!((gap - expected).abs() < 4.0, "line gap {gap:.1} != {expected:.1}");

        let _ = std::fs::remove_file(&out);
    }
}
