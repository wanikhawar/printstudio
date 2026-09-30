//! Draw pages and output sheets on screen with poppler + cairo.
//!
//! The preview never writes a PDF: output sheets are composed directly from
//! the source pages with the same geometry the PDF writer uses.

use std::path::Path;

use gtk::{gdk, glib};

use crate::geometry::{Matrix, SheetLayout, rotated_size};
use crate::pipeline::{Orientation, Scaling};
use crate::pipeline::{BLANK, Side};

pub struct Source {
    pub doc: poppler::Document,
    /// Page sizes as displayed (crop box, page rotation applied), in points.
    pub sizes: Vec<(f64, f64)>,
}

impl Source {
    pub fn open(path: &Path) -> Result<Source, String> {
        let uri = glib::filename_to_uri(path, None).map_err(|e| e.to_string())?;
        let doc = poppler::Document::from_file(&uri, None).map_err(|e| {
            if e.message().to_lowercase().contains("password") {
                "The PDF is password protected".to_string()
            } else {
                format!("Couldn't read the PDF: {}", e.message())
            }
        })?;
        let sizes = (0..doc.n_pages()).map(|i| doc.page(i).map(|p| p.size()).unwrap_or((612.0, 792.0))).collect();
        Ok(Source { doc, sizes })
    }

    pub fn n_pages(&self) -> usize {
        self.sizes.len()
    }
}

/// How one output sheet is made up.
#[derive(Clone, Copy)]
pub struct SheetSpec<'a> {
    pub side: &'a Side,
    /// Extra clockwise rotation of the sheet on paper.
    pub rotation: u32,
    pub layout: SheetLayout,
    /// Size for blank sides when no paper size is known.
    pub fallback: (f64, f64),
}

/// Shows pages exactly as they are in the document (no paper layout).
pub const AS_IS: SheetLayout = SheetLayout {
    nup: 1,
    paper: None,
    margin: 0.0,
    orientation: Orientation::Auto,
    scaling: Scaling::Fit,
    gutter: 0.0,
};

impl<'a> SheetSpec<'a> {
    /// One document page on its own, as it is in the file.
    pub fn page(side: &'a Side) -> SheetSpec<'a> {
        SheetSpec { side, rotation: 0, layout: AS_IS, fallback: (612.0, 792.0) }
    }
}

/// Sheet size before the sheet's own rotation.
fn sheet_size(src: &Source, spec: &SheetSpec) -> (f64, f64) {
    let first = spec.side.iter().find(|&&i| i != BLANK).and_then(|&i| src.sizes.get(i)).copied();
    spec.layout.sheet_size(first, spec.fallback)
}

/// Size of the sheet as it will look on paper, in points.
pub fn displayed_sheet_size(src: &Source, spec: &SheetSpec) -> (f64, f64) {
    let (w, h) = sheet_size(src, spec);
    rotated_size(w, h, spec.rotation)
}

fn cairo_matrix(m: Matrix) -> cairo::Matrix {
    let [a, b, c, d, e, f] = m.0;
    cairo::Matrix::new(a, b, c, d, e, f)
}

/// Clockwise rotation of a `w`×`h` sheet in y-down (screen) coordinates.
fn screen_rotation(w: f64, h: f64, rotation: u32) -> cairo::Matrix {
    match rotation % 360 {
        90 => cairo::Matrix::new(0.0, 1.0, -1.0, 0.0, h, 0.0),
        180 => cairo::Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
        270 => cairo::Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w),
        _ => cairo::Matrix::identity(),
    }
}

fn draw_sheet(cr: &cairo::Context, src: &Source, spec: &SheetSpec) {
    let (sw, sh) = sheet_size(src, spec);
    cr.transform(screen_rotation(sw, sh, spec.rotation));
    if spec.layout.passthrough() {
        if let Some(page) = spec.side.first().and_then(|&i| src.doc.page(i as i32)) {
            page.render(cr);
        }
        return;
    }
    // Anything outside the paper doesn't print, so don't show it either.
    cr.rectangle(0.0, 0.0, sw, sh);
    cr.clip();
    for (slot, &idx) in spec.side.iter().enumerate() {
        if idx == BLANK {
            continue;
        }
        let Some(page) = src.doc.page(idx as i32) else { continue };
        let (pw, ph) = src.sizes[idx];
        // poppler draws y-down; the shared geometry is y-up.
        let m = Matrix::flip_y(ph).then(spec.layout.place(slot, (sw, sh), (pw, ph))).then(Matrix::flip_y(sh));
        cr.save().ok();
        cr.transform(cairo_matrix(m));
        cr.rectangle(0.0, 0.0, pw, ph);
        cr.clip();
        page.render(cr);
        cr.restore().ok();
    }
}

/// Render a sheet so its longer edge is `max_px` device pixels.
pub fn render_sheet(src: &Source, spec: &SheetSpec, max_px: i32) -> Option<gdk::Texture> {
    let (dw, dh) = displayed_sheet_size(src, spec);
    let k = max_px as f64 / dw.max(dh).max(1.0);
    render_scaled(src, spec, k)
}

/// Render a sheet at `k` device pixels per point.
pub fn render_scaled(src: &Source, spec: &SheetSpec, k: f64) -> Option<gdk::Texture> {
    let (dw, dh) = displayed_sheet_size(src, spec);
    let (w, h) = ((dw * k).ceil().max(1.0) as i32, (dh * k).ceil().max(1.0) as i32);
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.scale(k, k);
        draw_sheet(&cr, src, spec);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().ok()?;
    let bytes = glib::Bytes::from(&data[..]);
    // Cairo's ARGB32 is B,G,R,A in memory on little-endian machines.
    Some(gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdfout;
    use crate::pipeline::{JobOptions, flatten, plan};
    use crate::testpdf::{A4, numbered};
    use gtk::prelude::*;

    #[allow(deprecated)]
    fn pixels(t: &gdk::Texture) -> (i32, i32, Vec<u8>) {
        let (w, h) = (t.width(), t.height());
        let mut buf = vec![0u8; (w * h * 4) as usize];
        t.download(&mut buf, (w * 4) as usize);
        (w, h, buf)
    }

    /// Average per-channel difference between two same-sized renders (0..255).
    fn diff(a: &gdk::Texture, b: &gdk::Texture) -> f64 {
        let (aw, ah, pa) = pixels(a);
        let (bw, bh, pb) = pixels(b);
        assert_eq!((aw, ah), (bw, bh), "size mismatch");
        pa.iter().zip(&pb).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / pa.len() as f64
    }

    /// The on-screen preview must match what the PDF writer produces, for
    /// n-up layouts, rotated source pages and rotated sheets alike.
    #[test]
    fn preview_matches_printed_pdf() {
        let dir = std::env::temp_dir().join(format!("printstudio-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut src_doc = numbered(7, A4);
        // Give page 2 a /Rotate so rotated sources are covered too.
        let p2 = *src_doc.get_pages().get(&2).unwrap();
        src_doc.get_object_mut(p2).unwrap().as_dict_mut().unwrap().set("Rotate", 90);
        let src_path = dir.join("src.pdf");
        src_doc.save(&src_path).unwrap();
        let src_lopdf = pdfout::load(&src_path).unwrap();
        let source = Source::open(&src_path).unwrap();
        assert_eq!(source.sizes[1], (842.0, 595.0), "poppler applies /Rotate to sizes");

        use crate::pipeline::{Orientation, Scaling};
        let letter = Some((612.0, 792.0));
        let a4 = Some(A4);
        // (options, paper, margin)
        let cases = [
            (JobOptions { reverse: false, ..Default::default() }, None, 0.0),
            (JobOptions { nup: 2, ..Default::default() }, letter, 0.0),
            (JobOptions { nup: 4, rotate: 90, ..Default::default() }, letter, 9.0),
            (JobOptions { nup: 6, ..Default::default() }, letter, 9.0),
            (JobOptions { duplex: true, backs_rotate: true, nup: 2, ..Default::default() }, letter, 9.0),
            (JobOptions { rotate: 270, ..Default::default() }, None, 0.0),
            // 1-up on A4: page 2 is landscape, so automatic orientation turns its sheet.
            (JobOptions::default(), a4, 9.0),
            (JobOptions { orientation: Orientation::Portrait, ..Default::default() }, a4, 9.0),
            (JobOptions { orientation: Orientation::Landscape, scaling: Scaling::Shrink, ..Default::default() }, a4, 9.0),
            (JobOptions { scaling: Scaling::Actual, rotate: 90, ..Default::default() }, letter, 9.0),
            (JobOptions { duplex: true, backs_rotate: true, ..Default::default() }, a4, 9.0),
            // Booklets: blank slots, the gutter, and backs turned for a short-edge flip.
            (JobOptions { booklet: true, gutter_mm: 8.0, ..Default::default() }, a4, 9.0),
            (JobOptions { booklet: true, booklet_rtl: true, booklet_sheets: 1, backs_rotate: true, ..Default::default() }, letter, 0.0),
        ];
        for (n, (opts, paper, margin)) in cases.iter().enumerate() {
            let layout = SheetLayout::new(opts, *paper, *margin);
            let passes = plan(source.n_pages(), opts).unwrap();
            let sides = flatten(&passes, opts);
            let out = dir.join(format!("out-{n}.pdf"));
            let mut doc = pdfout::build(&src_lopdf, &sides, &layout, pdfout::Target::Printer).unwrap();
            doc.save(&out).unwrap();
            let printed = Source::open(&out).unwrap();
            assert_eq!(printed.n_pages(), sides.len());
            let fallback = source.sizes[crate::pipeline::first_page(sides.iter().map(|(s, _)| s)).unwrap_or(0)];
            for (i, (side, rot)) in sides.iter().enumerate() {
                let spec = SheetSpec { side, rotation: *rot, layout, fallback };
                let preview = render_sheet(&source, &spec, 400).unwrap();
                let one = vec![i];
                // Landscape sheets are printed on portrait paper, turned; turn them back to compare.
                let (pw, ph) = displayed_sheet_size(&source, &spec);
                let (qw, qh) = printed.sizes[i];
                let turn_back = if (pw > ph) != (qw > qh) { 90 } else { 0 };
                let pdf_spec = SheetSpec { fallback, rotation: turn_back, ..SheetSpec::page(&one) };
                let actual = render_sheet(&printed, &pdf_spec, 400).unwrap();
                let d = diff(&preview, &actual);
                // A deliberately wrong render (turned upside down) must score far worse,
                // otherwise this comparison couldn't catch a misplaced page.
                let wrong_spec = SheetSpec { rotation: (turn_back + 180) % 360, ..pdf_spec };
                let wrong = render_sheet(&printed, &wrong_spec, 400).unwrap();
                if !side.is_empty() {
                    let dw = diff(&preview, &wrong);
                    assert!(dw > 0.01, "case {n} sheet {i}: an upside-down page only differs by {dw:.4}");
                }
                assert!(d < 0.002, "case {n} sheet {i} ({side:?} rot {rot}): preview differs from PDF by {d:.4}");
            }
            if paper.is_some() {
                // Whatever the orientation, the printer always gets portrait paper-sized pages.
                let (w, h) = paper.unwrap();
                assert!(printed.sizes.iter().all(|s| *s == (w, h)), "case {n}: {:?}", printed.sizes);
            }
            if n == 6 {
                // Automatic orientation: only the landscape page 2 is shown on landscape A4.
                let shown: Vec<(f64, f64)> = sides
                    .iter()
                    .map(|(side, rot)| displayed_sheet_size(&source, &SheetSpec { side, rotation: *rot, layout, fallback: A4 }))
                    .collect();
                assert_eq!(shown.iter().filter(|s| **s == (842.0, 595.0)).count(), 1, "{shown:?}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
