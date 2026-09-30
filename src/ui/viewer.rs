//! Full-size page viewer with zoom, opened by double-clicking a thumbnail.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::fit_to_monitor;

/// Renders page `index` at `k` device pixels per point.
pub type PageRenderer = Rc<dyn Fn(u32, f64) -> Option<gdk::Texture>>;
/// Displayed page size in points.
pub type PageSizer = Rc<dyn Fn(u32) -> (f64, f64)>;

/// Screen pixels per point at 100% (96 dpi).
const PX_PER_PT: f64 = 96.0 / 72.0;
const ZOOM_STEP: f64 = 1.25;
const MIN_ZOOM: f64 = 0.1;
const MAX_ZOOM: f64 = 4.0;
/// Largest texture edge we'll render, in device pixels.
const MAX_TEXTURE: f64 = 8192.0;
const MARGIN: f64 = 48.0;
/// Header bar height, for sizing before the first layout.
const HEADER_ESTIMATE: f64 = 50.0;

#[derive(Clone, Copy, PartialEq)]
enum Fit {
    Custom,
    Width,
    Page,
}

struct Viewer {
    window: adw::Window,
    title: adw::WindowTitle,
    scrolled: gtk::ScrolledWindow,
    picture: gtk::Picture,
    zoom_label: gtk::Button,
    prev: gtk::Button,
    next: gtk::Button,
    render: PageRenderer,
    size: PageSizer,
    labels: Vec<String>,
    count: u32,
    page: Cell<u32>,
    zoom: Cell<f64>,
    fit: Cell<Fit>,
    last_alloc: Cell<(i32, i32)>,
}

impl Viewer {
    fn effective_zoom(&self) -> f64 {
        let (w, h) = (self.size)(self.page.get());
        // Before the first layout the view has no size yet; use the window's.
        let (view_w, view_h) = match (self.scrolled.width(), self.scrolled.height()) {
            (w, h) if w > 0 && h > 0 => (w as f64, h as f64),
            _ => {
                let (w, h) = self.window.default_size();
                (w as f64, h as f64 - HEADER_ESTIMATE)
            }
        };
        let avail_w = (view_w - MARGIN).max(50.0);
        let avail_h = (view_h - MARGIN).max(50.0);
        let fit_w = avail_w / (w * PX_PER_PT).max(1.0);
        match self.fit.get() {
            Fit::Custom => self.zoom.get(),
            Fit::Width => fit_w,
            Fit::Page => fit_w.min(avail_h / (h * PX_PER_PT).max(1.0)),
        }
        .clamp(MIN_ZOOM, MAX_ZOOM)
    }

    fn render(&self) {
        let page = self.page.get();
        let (w, h) = (self.size)(page);
        let zoom = self.effective_zoom();
        self.zoom.set(zoom);
        let scale = super::device_scale(&self.window) as f64;
        let k = (zoom * PX_PER_PT * scale).min(MAX_TEXTURE / w.max(h).max(1.0));
        if let Some(tex) = (self.render)(page, k) {
            self.picture.set_paintable(Some(&tex));
        }
        super::set_picture_size(&self.picture, (w * zoom * PX_PER_PT).round() as i32, (h * zoom * PX_PER_PT).round() as i32);
        self.zoom_label.set_label(&format!("{}%", (zoom * 100.0).round()));
        let label = self.labels.get(page as usize).cloned().unwrap_or_default();
        self.title.set_subtitle(&format!("Page {} of {}{}", page + 1, self.count, if label.is_empty() { String::new() } else { format!(" · {label}") }));
        self.prev.set_sensitive(page > 0);
        self.next.set_sensitive(page + 1 < self.count);
    }

    /// Change zoom while keeping the same spot of the page in the middle of the view.
    fn zoom_to(&self, fit: Fit, zoom: f64) {
        let adj = self.scrolled.vadjustment();
        let centre = (adj.value() + adj.page_size() / 2.0) / adj.upper().max(1.0);
        self.fit.set(fit);
        self.zoom.set(zoom.clamp(MIN_ZOOM, MAX_ZOOM));
        self.render();
        let scrolled = self.scrolled.clone();
        glib::idle_add_local_once(move || {
            let adj = scrolled.vadjustment();
            adj.set_value(centre * adj.upper() - adj.page_size() / 2.0);
        });
    }

    fn zoom_by(&self, factor: f64) {
        self.zoom_to(Fit::Custom, self.effective_zoom() * factor);
    }

    fn go(&self, page: u32) {
        if page < self.count && page != self.page.get() {
            self.page.set(page);
            self.render();
            self.scrolled.vadjustment().set_value(0.0);
        }
    }
}

fn button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b
}

pub fn open(parent: &impl IsA<gtk::Window>, title: &str, count: u32, start: u32,
            render: PageRenderer, size: PageSizer, labels: Vec<String>) {
    let window = adw::Window::builder().modal(true).transient_for(parent).destroy_with_parent(true).build();
    fit_to_monitor(window.upcast_ref(), Some(parent.upcast_ref()), 900, 1000, 0.85);

    let title_widget = adw::WindowTitle::new(title, "");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title_widget));
    let prev = button("go-previous-symbolic", "Previous page (←)");
    let next = button("go-next-symbolic", "Next page (→)");
    header.pack_start(&prev);
    header.pack_start(&next);

    let zoom_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    zoom_box.add_css_class("linked");
    let zoom_out = button("zoom-out-symbolic", "Zoom out (Ctrl+−)");
    let zoom_label = gtk::Button::with_label("100%");
    zoom_label.set_tooltip_text(Some("Actual size"));
    let zoom_in = button("zoom-in-symbolic", "Zoom in (Ctrl++)");
    zoom_box.append(&zoom_out);
    zoom_box.append(&zoom_label);
    zoom_box.append(&zoom_in);
    let fit_width = button("zoom-fit-best-symbolic", "Fit width");
    let fit_page = button("view-fullscreen-symbolic", "Fit page (Ctrl+0)");
    header.pack_end(&fit_page);
    header.pack_end(&fit_width);
    header.pack_end(&zoom_box);

    let (frame, picture) = super::sized_picture();
    frame.set_halign(gtk::Align::Center);
    frame.set_valign(gtk::Align::Center);
    frame.set_margin_top(24);
    frame.set_margin_bottom(24);
    frame.set_margin_start(24);
    frame.set_margin_end(24);
    let scrolled = gtk::ScrolledWindow::builder().child(&frame).vexpand(true).hexpand(true).build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled));
    window.set_content(Some(&toolbar));

    let v = Rc::new(Viewer {
        window: window.clone(),
        title: title_widget,
        scrolled: scrolled.clone(),
        picture,
        zoom_label: zoom_label.clone(),
        prev: prev.clone(),
        next: next.clone(),
        render,
        size,
        labels,
        count,
        page: Cell::new(start.min(count.saturating_sub(1))),
        zoom: Cell::new(1.0),
        fit: Cell::new(Fit::Page),
        last_alloc: Cell::new((0, 0)),
    });

    prev.connect_clicked(glib::clone!(#[weak] v, move |_| v.go(v.page.get().saturating_sub(1))));
    next.connect_clicked(glib::clone!(#[weak] v, move |_| v.go(v.page.get() + 1)));
    zoom_out.connect_clicked(glib::clone!(#[weak] v, move |_| v.zoom_by(1.0 / ZOOM_STEP)));
    zoom_in.connect_clicked(glib::clone!(#[weak] v, move |_| v.zoom_by(ZOOM_STEP)));
    zoom_label.connect_clicked(glib::clone!(#[weak] v, move |_| v.zoom_to(Fit::Custom, 1.0)));
    fit_width.connect_clicked(glib::clone!(#[weak] v, move |_| v.zoom_to(Fit::Width, 1.0)));
    fit_page.connect_clicked(glib::clone!(#[weak] v, move |_| v.zoom_to(Fit::Page, 1.0)));

    // Ctrl+scroll zooms; plain scrolling scrolls.
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll.connect_scroll(glib::clone!(#[weak] v, #[upgrade_or] glib::Propagation::Proceed, move |ctl, _, dy| {
        if ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
            v.zoom_by(if dy < 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP });
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }));
    scrolled.add_controller(scroll);

    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Managed);
    let add = |trigger: &str, f: Box<dyn Fn(&Viewer)>| {
        let weak = Rc::downgrade(&v);
        let action = gtk::CallbackAction::new(move |_, _| {
            if let Some(v) = weak.upgrade() {
                f(&v);
            }
            glib::Propagation::Stop
        });
        shortcuts.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(action)));
    };
    add("Escape", Box::new(|v| v.window.close()));
    add("<Control>plus|<Control>equal|<Control>KP_Add", Box::new(|v| v.zoom_by(ZOOM_STEP)));
    add("<Control>minus|<Control>KP_Subtract", Box::new(|v| v.zoom_by(1.0 / ZOOM_STEP)));
    add("<Control>0", Box::new(|v| v.zoom_to(Fit::Page, 1.0)));
    add("Left|Page_Up", Box::new(|v| v.go(v.page.get().saturating_sub(1))));
    add("Right|Page_Down", Box::new(|v| v.go(v.page.get() + 1)));
    window.add_controller(shortcuts);

    // Re-fit when the window changes size (and once it first has a size).
    scrolled.add_tick_callback(glib::clone!(#[weak] v, #[upgrade_or] glib::ControlFlow::Break, move |s, _| {
        let alloc = (s.width(), s.height());
        if alloc != v.last_alloc.get() && alloc.0 > 0 {
            v.last_alloc.set(alloc);
            if v.fit.get() != Fit::Custom {
                v.render();
            }
        }
        glib::ControlFlow::Continue
    }));

    v.render();
    window.present();
}
