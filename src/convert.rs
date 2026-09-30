//! Turn whatever the user opens into a PDF.

use std::path::{Path, PathBuf};
use std::process::Command;

use gtk::gdk_pixbuf::Pixbuf;
use gtk::prelude::*;

const IMAGE_SUFFIXES: [&str; 8] = ["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"];
const A4_INCHES: (f64, f64) = (8.27, 11.69);

pub fn is_pdf(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 1024];
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let n = f.read(&mut head).unwrap_or(0);
    head[..n].trim_ascii_start().starts_with(b"%PDF")
}

fn image_to_pdf(src: &Path, dest: &Path) -> Result<(), String> {
    let pixbuf = Pixbuf::from_file(src).map_err(|e| format!("Couldn't read image {}: {e}", src.display()))?;
    let (w, h) = (pixbuf.width() as f64, pixbuf.height() as f64);
    // Choose a resolution that makes the image about A4-sized rather than one point per pixel.
    let dpi = (w / A4_INCHES.0).max(h / A4_INCHES.1).max(72.0);
    let scale = 72.0 / dpi;
    let surface = cairo::PdfSurface::new(w * scale, h * scale, dest).map_err(|e| e.to_string())?;
    let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
    cr.scale(scale, scale);
    #[allow(deprecated)]
    cr.set_source_pixbuf(&pixbuf, 0.0, 0.0);
    cr.paint().map_err(|e| e.to_string())?;
    drop(cr);
    surface.finish();
    Ok(())
}

fn office_to_pdf(src: &Path, workdir: &Path) -> Result<PathBuf, String> {
    let soffice = ["soffice", "libreoffice"]
        .into_iter()
        .find(|b| Command::new("which").arg(b).output().is_ok_and(|o| o.status.success()))
        .ok_or("LibreOffice is needed to print this kind of file.")?;
    let out = Command::new(soffice)
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(workdir)
        .arg(src)
        .output()
        .map_err(|e| e.to_string())?;
    let pdf = workdir.join(src.file_stem().unwrap_or_default()).with_extension("pdf");
    if !out.status.success() || !pdf.exists() {
        return Err(format!(
            "LibreOffice couldn't convert {}:\n{}",
            src.file_name().unwrap_or_default().to_string_lossy(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(pdf)
}

/// A PDF version of `src`: `src` itself if it's already a PDF.
pub fn to_pdf(src: &Path, workdir: &Path) -> Result<PathBuf, String> {
    if is_pdf(src) {
        return Ok(src.to_path_buf());
    }
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if IMAGE_SUFFIXES.contains(&ext.as_str()) {
        let dest = workdir.join(src.file_stem().unwrap_or_default()).with_extension("pdf");
        image_to_pdf(src, &dest)?;
        return Ok(dest);
    }
    office_to_pdf(src, workdir)
}
