//! The GTK4 / libadwaita interface.

mod art;
mod palette;
mod thumbs;
mod viewer;
mod window;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use printstudio::spool::SpoolJob;
use window::Incoming;

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

/// Make the app icon available when running from the source tree too.
fn add_icon_path() {
    let Some(display) = gdk::Display::default() else { return };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data").join("icons");
    if dir.is_dir() {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

const SHORTCUTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="shortcuts">
    <property name="modal">1</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">main</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Printing</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Print</property><property name="accelerator">&lt;Control&gt;p</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Add a document</property><property name="accelerator">&lt;Control&gt;o</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Save as PDF</property><property name="accelerator">&lt;Control&gt;&lt;Shift&gt;s</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Close</property><property name="accelerator">Escape</property></object></child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">View</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Bigger thumbnails</property><property name="accelerator">&lt;Control&gt;plus</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Smaller thumbnails</property><property name="accelerator">&lt;Control&gt;minus</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Show or hide settings</property><property name="accelerator">F9</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Keyboard shortcuts</property><property name="accelerator">&lt;Control&gt;question</property></object></child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Page viewer</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Previous / next page</property><property name="accelerator">Left Right</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Zoom in / out</property><property name="accelerator">&lt;Control&gt;plus &lt;Control&gt;minus</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Fit page</property><property name="accelerator">&lt;Control&gt;0</property></object></child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>"#;

pub fn show_shortcuts(parent: &gtk::Window) {
    let builder = gtk::Builder::from_string(SHORTCUTS);
    if let Some(window) = builder.object::<gtk::ShortcutsWindow>("shortcuts") {
        window.set_transient_for(Some(parent));
        window.present();
    }
}

/// `args`: what main() was given, already checked: `[]`, `[FILE]` or `["--spool-job", JOB]`.
///
/// Only one Print Studio runs at a time. Starting it again (or the watcher
/// opening a new job) hands the arguments to the running copy, which puts
/// them in an empty window or asks whether to add them to the job on screen.
pub fn run(args: Vec<String>) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| {
        tune_font_rendering();
        load_css();
        add_icon_path();
    });
    app.connect_command_line(|app, cmdline| {
        let args: Vec<String> = cmdline.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let item = match args.as_slice() {
            [flag, job] if flag == "--spool-job" => Some(Incoming::Spool(SpoolJob::load(&PathBuf::from(job)))),
            [file] => cmdline.create_file_for_arg(file).path().map(Incoming::File),
            _ => None,
        };
        #[cfg(feature = "devshot")]
        if let (false, Ok(script)) = (cmdline.is_remote(), std::env::var("PRINTSTUDIO_DEVSCRIPT")) {
            let win = window::Win::new(app, item);
            win.present();
            win.run_dev_script(script);
            return glib::ExitCode::SUCCESS;
        }
        window::deliver(app, item);
        glib::ExitCode::SUCCESS
    });
    app.set_accels_for_action("win.print", &["<Control>p"]);
    app.set_accels_for_action("win.open", &["<Control>o"]);
    app.set_accels_for_action("win.save-pdf", &["<Control><Shift>s"]);
    app.set_accels_for_action("win.toggle-sidebar", &["F9"]);
    app.set_accels_for_action("win.shortcuts", &["<Control>question"]);
    app.set_accels_for_action("win.zoom-in", &["<Control>plus", "<Control>equal", "<Control>KP_Add"]);
    app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
    app.set_accels_for_action("win.escape", &["Escape"]);
    let mut argv = vec!["printstudio".to_string()];
    argv.extend(args);
    app.run_with_args(&argv)
}
