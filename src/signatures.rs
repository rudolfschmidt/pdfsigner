//! Discovery of signature images. The directory comes from the
//! `PDFSIGNER_SIGNATURES` environment variable; if it isn't set, the
//! signature menu is simply empty (no config file).

use std::path::PathBuf;

const ENV_VAR: &str = "PDFSIGNER_SIGNATURES";

/// PNG/JPG/JPEG files in `$PDFSIGNER_SIGNATURES`, sorted by name. Returns an
/// empty `Vec` if the env var is unset or the directory is missing/empty.
pub fn list_signatures() -> Vec<PathBuf> {
    let Some(dir) = std::env::var_os(ENV_VAR).map(PathBuf::from) else {
        return vec![];
    };
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
