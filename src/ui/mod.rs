//! The GTK4 / libadwaita interface.

mod palette;
mod thumbs;
mod viewer;
mod window;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use printstudio::spool::SpoolJob;

pub const APP_ID: &str = "dev.printstudio.PrintStudio";

/// "system" | "light" | "dark"
pub fn apply_color_scheme(scheme: &str) {
    let (adw_scheme, want_dark) = match scheme {
        "light" => (adw::ColorScheme::ForceLight, Some(false)),
        "dark" => (adw::ColorScheme::ForceDark, Some(true)),
        _ => (adw::ColorScheme::Default, None), // follows the desktop's light/dark preference
    };
    adw::StyleManager::default().set_color_scheme(adw_scheme);
    palette::sync(want_dark);
}

/// Default size, but never more than `fraction` of the monitor (in logical
/// pixels, so display scaling and panels are allowed for).
pub fn fit_to_monitor(window: &gtk::Window, parent: Option<&gtk::Window>, width: i32, height: i32, fraction: f64) {
    let display = gdk::Display::default();
    let monitor = parent
        .and_then(|p| p.surface())
        .and_then(|s| display.as_ref()?.monitor_at_surface(&s))
        .or_else(|| display.as_ref()?.monitors().item(0).and_downcast::<gdk::Monitor>());
    let (w, h) = match monitor {
        Some(m) => {
            let g = m.geometry();
            (width.min((g.width() as f64 * fraction) as i32), height.min((g.height() as f64 * fraction) as i32))
        }
        None => (width, height),
    };
    window.set_default_size(w, h);
}

/// Device pixels per logical pixel for a widget (2 on HiDPI and fractional-scaled screens).
pub fn device_scale(widget: &impl IsA<gtk::Widget>) -> i32 {
    #[cfg(feature = "devshot")]
    if let Some(forced) = std::env::var("PRINTSTUDIO_FORCE_SCALE").ok().and_then(|v| v.parse().ok()) {
        return forced;
    }
    widget.scale_factor()
}

/// A picture wrapped so it's shown at exactly the logical size we give it.
///
/// GtkPicture's natural size is the texture's pixel size; on HiDPI screens we
/// render at 2x, which would make the white "paper" wider than the page drawn
/// on it. Two clamps cap the size in each direction.
pub fn sized_picture() -> (adw::Clamp, gtk::Picture) {
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Fill);
    picture.add_css_class("page");
    let vertical = adw::Clamp::builder().orientation(gtk::Orientation::Vertical).child(&picture).build();
    let horizontal = adw::Clamp::builder().orientation(gtk::Orientation::Horizontal).child(&vertical).build();
    (horizontal, picture)
}

/// Set the on-screen size of a picture made by `sized_picture`.
pub fn set_picture_size(picture: &gtk::Picture, w: i32, h: i32) {
    picture.set_size_request(w, h);
    if let Some(vertical) = picture.parent().and_downcast::<adw::Clamp>() {
        vertical.set_maximum_size(h);
        vertical.set_tightening_threshold(h);
        if let Some(horizontal) = vertical.parent().and_downcast::<adw::Clamp>() {
            horizontal.set_maximum_size(w);
            horizontal.set_tightening_threshold(w);
        }
    }
}

/// Turn off font hinting on fractionally scaled screens (e.g. 125%).
///
/// Hinted glyphs are fitted to whole pixels, but at a fractional scale text
/// usually lands between pixels, and thin horizontal strokes (the tops of 5
/// and 7) get split across two rows and almost vanish. Unhinted text renders
/// cleanly at any position. Only affects this app.
fn tune_font_rendering() {
    let (Some(display), Some(settings)) = (gdk::Display::default(), gtk::Settings::default()) else { return };
    let monitors = display.monitors();
    let fractional = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .any(|m| m.scale().fract().abs() > 1e-6);
    if fractional || std::env::var_os("PRINTSTUDIO_NO_HINTING").is_some() {
        settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
        settings.set_gtk_xft_hintstyle(Some("hintnone"));
        settings.set_gtk_hint_font_metrics(false);
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

pub fn run(file: Option<PathBuf>, spool: Option<SpoolJob>) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        // Each print job gets its own window and process.
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| {
        tune_font_rendering();
        load_css();
    });
    app.connect_activate(move |app| {
        let win = window::Win::new(app, file.clone(), spool.clone());
        win.present();
        #[cfg(feature = "devshot")]
        if let Ok(script) = std::env::var("PRINTSTUDIO_DEVSCRIPT") {
            win.run_dev_script(script);
        }
    });
    app.set_accels_for_action("win.print", &["<Control>p"]);
    app.set_accels_for_action("win.open", &["<Control>o"]);
    app.set_accels_for_action("win.zoom-in", &["<Control>plus", "<Control>equal", "<Control>KP_Add"]);
    app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
    app.set_accels_for_action("win.escape", &["Escape"]);
    // Our own arguments were handled in main(); don't let GApplication see them.
    app.run_with_args(&["printstudio"])
}
