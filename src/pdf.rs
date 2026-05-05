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

const INTER_TTF: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
/// Inter-Regular cap-height as a fraction of font size (used to position the
/// PDF text-baseline so saved output lines up with the on-screen preview).
const INTER_ASCENT: f32 = 0.728;

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
// Saving
// ----------------------------------------------------------------------------

/// Write `<input>_signed.pdf` (or `_signed_N.pdf` if it exists) with `overlays`
/// applied to each page. `pages_filter` like "1-3,5" keeps only those pages
/// in the output; empty string keeps all.
pub fn save(
    pdfium: Option<&Pdfium>,
    input: &Path,
    overlays_per_page: &[Vec<Overlay>],
    page_size_pt: &[(f32, f32)],
    pages_filter: &str,
) -> Result<PathBuf> {
    let output = unique_output_path(input);

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
        let (_, page_h_pt) = *page_size_pt.get(idx).unwrap_or(&(595.0, 842.0));
        write_overlays_for_page(&mut doc, page_id, page_h_pt, overlays, &mut inter_font_id)?;
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

fn unique_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "out".into());
    let parent = input.parent().unwrap_or(Path::new("."));
    let mut output = parent.join(format!("{stem}_signed.pdf"));
    let mut n = 1;
    while output.exists() {
        output = parent.join(format!("{stem}_signed_{n}.pdf"));
        n += 1;
    }
    output
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
    page_h_pt: f32,
    overlays: &[Overlay],
    inter_font_id: &mut Option<ObjectId>,
) -> Result<()> {
    // Pick resource names that don't collide with the source PDF — overwriting
    // an existing /F1 or /Im1 corrupts the original glyph mapping.
    let existing_fonts = page_resource_keys(doc, page_id, b"Font");
    let mut used_xobj = page_resource_keys(doc, page_id, b"XObject");
    let font_name = pick_free_name(&existing_fonts, "F");

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
                let pdf_y = page_h_pt - y - h;
                content.extend_from_slice(
                    format!(
                        "q\n{:.4} 0 0 {:.4} {:.4} {:.4} cm\n/{} Do\nQ\n",
                        w, h, x, pdf_y, res_name
                    )
                    .as_bytes(),
                );
                images.push((res_name.into_bytes(), img_id));
            }
            Overlay::Text { text, x, y, size_pt, color } => {
                text_used = true;
                let baseline = y + size_pt * INTER_ASCENT;
                let pdf_y = page_h_pt - baseline;
                let r = color[0] as f32 / 255.0;
                let g = color[1] as f32 / 255.0;
                let b = color[2] as f32 / 255.0;
                content.extend_from_slice(
                    format!(
                        "BT\n/{} {:.4} Tf\n{:.4} {:.4} {:.4} rg\n{:.4} {:.4} Td\n",
                        font_name, size_pt, r, g, b, x, pdf_y
                    )
                    .as_bytes(),
                );
                content.extend_from_slice(b"<");
                content.extend_from_slice(encode_text_for_inter(text).as_bytes());
                content.extend_from_slice(b"> Tj\nET\n");
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
    out.sort_unstable();
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
