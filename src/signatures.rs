//! Discovery of signature images from `~/.config/pdfsigner/signatures`.

use std::path::PathBuf;

const DIR: &str = ".config/pdfsigner/signatures";

fn signatures_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(DIR))
}

/// PNG/JPG/JPEG files in the signatures dir, sorted by name. Creates the
/// directory if it doesn't exist; returns empty on any failure.
pub fn list_signatures() -> Vec<PathBuf> {
    let Some(dir) = signatures_dir() else { return vec![] };
    let _ = std::fs::create_dir_all(&dir);
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let is_img = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
                .unwrap_or(false);
            if is_img {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}
