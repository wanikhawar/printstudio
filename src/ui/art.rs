//! Small illustrations for the two-sided and booklet instructions, drawn
//! with cairo in the current text colour so they suit light and dark mode.

use std::f64::consts::PI;

use gtk::prelude::*;

const ACCENT: (f64, f64, f64) = (0.21, 0.52, 0.89);

fn fg(widget: &gtk::DrawingArea) -> (f64, f64, f64) {
    let c = widget.color();
    (c.red() as f64, c.green() as f64, c.blue() as f64)
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

/// A sheet of paper at (x, y), with a few lines of "text" if `printed`.
fn sheet(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, printed: bool) {
    rounded(cr, x, y, w, h, 3.0);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill_preserve().ok();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
    cr.set_line_width(1.0);
    cr.stroke().ok();
    if printed {
        cr.set_source_rgb(ACCENT.0, ACCENT.1, ACCENT.2);
        cr.rectangle(x + 5.0, y + 6.0, w - 10.0, 5.0);
        cr.fill().ok();
        // Paper is white in both modes, so the "text" is always dark.
        cr.set_source_rgba(0.3, 0.3, 0.33, 0.55);
        let mut ly = y + 16.0;
        while ly < y + h - 6.0 {
            cr.rectangle(x + 5.0, ly, (w - 10.0) * if ((ly as i32) / 5) % 3 == 2 { 0.6 } else { 1.0 }, 1.6);
            ly += 5.0;
        }
        cr.fill().ok();
    }
}

fn arrow(cr: &cairo::Context, x0: f64, y0: f64, x1: f64, y1: f64, bend: f64, colour: (f64, f64, f64)) {
    cr.set_source_rgb(colour.0, colour.1, colour.2);
    cr.set_line_width(2.5);
    cr.set_line_cap(cairo::LineCap::Round);
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0 - bend);
    cr.move_to(x0, y0);
    cr.curve_to(mx, my, mx, my, x1, y1);
    cr.stroke().ok();
    // Arrow head along the last segment's direction.
    let angle = (y1 - my).atan2(x1 - mx);
    cr.move_to(x1, y1);
    cr.line_to(x1 - 10.0 * (angle - 0.45).cos(), y1 - 10.0 * (angle - 0.45).sin());
    cr.move_to(x1, y1);
    cr.line_to(x1 - 10.0 * (angle + 0.45).cos(), y1 - 10.0 * (angle + 0.45).sin());
    cr.stroke().ok();
}

fn caption(cr: &cairo::Context, text: &str, cx: f64, y: f64, colour: (f64, f64, f64)) {
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(11.0);
    cr.set_source_rgba(colour.0, colour.1, colour.2, 0.7);
    if let Ok(ext) = cr.text_extents(text) {
        cr.move_to(cx - ext.width() / 2.0 - ext.x_bearing(), y);
        cr.show_text(text).ok();
    }
}

fn area(width: i32, height: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(width);
    area.set_content_height(height);
    area.set_halign(gtk::Align::Center);
    area.set_margin_bottom(6);
    area
}

/// The printed stack goes from the output tray back into the paper tray.
pub fn reload() -> gtk::DrawingArea {
    let area = area(300, 130);
    area.set_draw_func(|a, cr, w, _| {
        let ink = fg(a);
        let cx = w as f64 / 2.0;
        // Output tray: a stack of printed fronts.
        for i in 0..4 {
            let o = i as f64 * 3.0;
            sheet(cr, cx - 120.0 + o, 18.0 + o, 62.0, 84.0, i == 3);
        }
        caption(cr, "Printed fronts", cx - 86.0, 122.0, ink);
        arrow(cr, cx - 40.0, 46.0, cx + 42.0, 46.0, 34.0, ACCENT);
        // Paper tray: the same stack, blank sides up.
        for i in 0..4 {
            let o = i as f64 * 3.0;
            sheet(cr, cx + 52.0 + o, 18.0 + o, 62.0, 84.0, false);
        }
        caption(cr, "Back in the tray", cx + 86.0, 122.0, ink);
    });
    area
}

/// A flat sheet with its fold line, then the folded booklet.
pub fn fold(booklets: usize) -> gtk::DrawingArea {
    let area = area(320, 140);
    area.set_draw_func(move |a, cr, w, _| {
        let ink = fg(a);
        let cx = w as f64 / 2.0;
        // Open landscape sheet, pages 8 | 1.
        let (sx, sy, sw, sh) = (cx - 150.0, 26.0, 124.0, 84.0);
        sheet(cr, sx, sy, sw / 2.0, sh, true);
        sheet(cr, sx + sw / 2.0, sy, sw / 2.0, sh, true);
        cr.set_source_rgb(ACCENT.0, ACCENT.1, ACCENT.2);
        cr.set_line_width(1.5);
        cr.set_dash(&[4.0, 3.0], 0.0);
        cr.move_to(sx + sw / 2.0, sy - 8.0);
        cr.line_to(sx + sw / 2.0, sy + sh + 8.0);
        cr.stroke().ok();
        cr.set_dash(&[], 0.0);
        caption(cr, "Fold along the middle", sx + sw / 2.0, 132.0, ink);
        arrow(cr, cx - 14.0, 62.0, cx + 34.0, 62.0, 22.0, ACCENT);
        // The folded booklet, slightly open: back cover behind, front cover in front.
        let (bx, by, bw, bh) = (cx + 58.0, 22.0, 62.0, 88.0);
        for k in 0..booklets.clamp(1, 3) {
            let o = k as f64 * 6.0;
            cr.save().ok();
            cr.translate(o, -o);
            cr.move_to(bx, by);
            cr.line_to(bx + bw * 0.92, by - 6.0);
            cr.line_to(bx + bw * 0.92, by + bh - 6.0);
            cr.line_to(bx, by + bh);
            cr.close_path();
            cr.set_source_rgb(0.93, 0.93, 0.95);
            cr.fill_preserve().ok();
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
            cr.set_line_width(1.0);
            cr.stroke().ok();
            sheet(cr, bx, by, bw, bh, true);
            cr.restore().ok();
        }
        caption(cr, if booklets > 1 { "Fold each booklet, then stack them" } else { "Your booklet" }, bx + bw / 2.0, 132.0, ink);
    });
    area
}
