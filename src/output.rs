//! Output sinks for the flattened image: clipboard and file.

use crate::export::Flattened;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Copy the flattened image to the system clipboard.
pub fn copy_to_clipboard(f: &Flattened) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard: {e}"))?;
    cb.set_image(arboard::ImageData {
        width: f.width as usize,
        height: f.height as usize,
        bytes: Cow::Borrowed(&f.rgba),
    })
    .map_err(|e| format!("clipboard set image: {e}"))
}

/// Show a native save dialog and write the image. Returns the chosen path, or
/// `None` if the user cancelled.
pub fn save_with_dialog(
    f: &Flattened,
    default_name: &str,
    start_dir: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let mut dialog = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter("PNG image", &["png"])
        .add_filter("JPEG image", &["jpg", "jpeg"]);
    if let Some(dir) = start_dir {
        dialog = dialog.set_directory(dir);
    }
    let Some(path) = dialog.save_file() else {
        return Ok(None);
    };
    write_image(f, &path)?;
    Ok(Some(path))
}

/// Encode and write the flattened image to `path` (format inferred from extension).
pub fn write_image(f: &Flattened, path: &Path) -> Result<(), String> {
    let img = image::RgbaImage::from_raw(f.width, f.height, f.rgba.clone())
        .ok_or("invalid image buffer")?;
    let is_jpeg = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
        .unwrap_or(false);

    if is_jpeg {
        // JPEG has no alpha channel; drop it.
        let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
        rgb.save(path).map_err(|e| format!("save: {e}"))
    } else {
        img.save(path).map_err(|e| format!("save: {e}"))
    }
}
