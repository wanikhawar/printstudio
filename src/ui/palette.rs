//! Make the Light/Dark choice stick even when the desktop forces its own colours.
//!
//! Desktop theming tools (Noctalia, matugen, pywal…) write a
//! `~/.config/gtk-4.0/gtk.css` that hard-codes one palette for every GTK app.
//! GTK gives that file priority over libadwaita, so switching libadwaita to
//! light still shows the desktop's dark colours. When the user picks a mode
//! that disagrees with those colours, we lay libadwaita's standard palette
//! for that mode on top, just for this app.

use std::cell::RefCell;

use gtk::gdk;
use gtk::prelude::*;

/// (name, light, dark): libadwaita's standard colours.
const PALETTE: &[(&str, &str, &str)] = &[
    ("window_bg_color", "#fafafb", "#222226"),
    ("window_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("view_bg_color", "#ffffff", "#1d1d20"),
    ("view_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("headerbar_bg_color", "#ffffff", "#2e2e32"),
    ("headerbar_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("headerbar_backdrop_color", "#fafafb", "#222226"),
    ("popover_bg_color", "#ffffff", "#36363a"),
    ("popover_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("card_bg_color", "#ffffff", "rgba(255, 255, 255, 0.08)"),
    ("card_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("dialog_bg_color", "#fafafb", "#36363a"),
    ("dialog_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("overview_bg_color", "#f3f3f5", "#28282c"),
    ("overview_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("sidebar_bg_color", "#ebebed", "#2e2e32"),
    ("sidebar_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("sidebar_backdrop_color", "#f2f2f4", "#28282c"),
    ("sidebar_border_color", "rgba(0, 0, 6, 0.07)", "rgba(0, 0, 6, 0.36)"),
    ("secondary_sidebar_bg_color", "#f3f3f5", "#28282c"),
    ("secondary_sidebar_fg_color", "rgba(0, 0, 6, 0.8)", "#ffffff"),
    ("accent_bg_color", "#3584e4", "#3584e4"),
    ("accent_fg_color", "#ffffff", "#ffffff"),
    ("accent_color", "#0461be", "#81b7f5"),
    ("destructive_bg_color", "#e01b24", "#c01c28"),
    ("destructive_fg_color", "#ffffff", "#ffffff"),
    ("destructive_color", "#c30000", "#ff938c"),
    ("error_bg_color", "#e01b24", "#c01c28"),
    ("error_fg_color", "#ffffff", "#ffffff"),
    ("error_color", "#c30000", "#ff938c"),
    ("warning_bg_color", "#e5a50a", "#cd9309"),
    ("warning_fg_color", "rgba(0, 0, 0, 0.8)", "rgba(0, 0, 0, 0.8)"),
    ("warning_color", "#9c6e03", "#f8e45c"),
    ("success_bg_color", "#2ec27e", "#26a269"),
    ("success_fg_color", "#ffffff", "#ffffff"),
    ("success_color", "#007c3d", "#78e9ab"),
    ("shade_color", "rgba(0, 0, 6, 0.07)", "rgba(0, 0, 6, 0.36)"),
];

thread_local! {
    static OVERRIDE: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

fn pick(entry: &(&'static str, &'static str, &'static str), dark: bool) -> &'static str {
    if dark { entry.2 } else { entry.1 }
}

fn stylesheet(dark: bool) -> String {
    let mut css = String::new();
    for entry in PALETTE {
        css += &format!("@define-color {} {};\n", entry.0, pick(entry, dark));
    }
    css += ":root {\n";
    for entry in PALETTE {
        css += &format!("  --{}: {};\n", entry.0.replace('_', "-"), pick(entry, dark));
    }
    css + "}\n"
}

/// The window background colour after every stylesheet (including the desktop's) is applied.
#[allow(deprecated)] // lookup_color is the only way to read a resolved named colour
fn resolved_window_is_dark() -> Option<bool> {
    let probe = gtk::Label::new(None);
    let bg = probe.style_context().lookup_color("window_bg_color")?;
    let luminance = 0.2126 * bg.red() + 0.7152 * bg.green() + 0.0722 * bg.blue();
    Some(luminance < 0.5)
}

/// Called after libadwaita's colour scheme is set. `want_dark` is None for "follow system".
pub fn sync(want_dark: Option<bool>) {
    let Some(display) = gdk::Display::default() else { return };
    OVERRIDE.with(|o| {
        if let Some(old) = o.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
    });
    let Some(want_dark) = want_dark else { return }; // follow the desktop, colours and all
    if resolved_window_is_dark() == Some(want_dark) {
        return; // the desktop's colours already match; keep them
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&stylesheet(want_dark));
    // Above the user's gtk.css (which sits at USER priority).
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
    OVERRIDE.with(|o| *o.borrow_mut() = Some(provider));
}

#[cfg(test)]
mod tests {
    #[test]
    fn stylesheet_defines_named_colours_and_variables() {
        let css = super::stylesheet(false);
        assert!(css.contains("@define-color window_bg_color #fafafb;"));
        assert!(css.contains("--window-bg-color: #fafafb;"));
        assert!(super::stylesheet(true).contains("--window-bg-color: #222226;"));
    }
}
