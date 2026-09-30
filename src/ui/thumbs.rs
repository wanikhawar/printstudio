//! A zoomable grid of page thumbnails.
//!
//! GridView binds more items than are on screen (on a hidden tab it binds
//! them all), so binding only records what each card should show. Rendering
//! happens separately, for cards actually inside the visible area, in short
//! time slices so the window never stalls, even on huge scanned documents.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, glib};

/// Renders item `pos` so its longer edge is `px` device pixels.
pub type Renderer = Box<dyn Fn(u32, i32) -> Option<gdk::Texture>>;
/// Displayed size of item `pos`, in points (only the aspect ratio matters).
pub type Sizer = Box<dyn Fn(u32) -> (f64, f64)>;

pub const MIN_SIZE: i32 = 90;
pub const MAX_SIZE: i32 = 720;
/// Rendered thumbnails kept around; beyond this the cache starts over.
const CACHE_LIMIT: usize = 400;
/// Longest stretch spent rendering before letting the window handle events.
const SLICE: Duration = Duration::from_millis(25);
/// How often to look for cards to render while things are changing.
const TICK: Duration = Duration::from_millis(16);
/// Keep looking for this many quiet ticks after the last change: cards are
/// bound before GTK has laid them out, so the first look can come too early.
const SETTLE_TICKS: u32 = 6;

/// What one rendering pass found.
struct Progress {
    rendered: bool,
    /// Ran out of time with visible cards still waiting.
    more: bool,
    /// Some cards are shown but not laid out yet.
    unsettled: bool,
}

/// A card currently showing page `pos`.
#[derive(Clone)]
struct Card {
    pos: u32,
    item: glib::WeakRef<gtk::ListItem>,
    card: glib::WeakRef<gtk::Box>,
    picture: glib::WeakRef<gtk::Picture>,
}

impl Card {
    fn is_current(&self) -> bool {
        self.item.upgrade().is_some_and(|it| it.position() == self.pos) && self.picture.upgrade().is_some()
    }
}

pub struct Thumbs {
    pub scrolled: gtk::ScrolledWindow,
    pub view: gtk::GridView,
    pub model: gtk::StringList,
    pub selection: Option<gtk::MultiSelection>,
    size: Cell<i32>,
    cache: RefCell<HashMap<u32, gdk::Texture>>,
    renderer: RefCell<Option<Renderer>>,
    sizer: RefCell<Option<Sizer>>,
    /// Cards on (or near) screen, so a zoom can resize them in place.
    bound: RefCell<Vec<Card>>,
    /// Cards still waiting for their image.
    pending: RefCell<Vec<Card>>,
    render_scheduled: Cell<bool>,
    quiet_ticks: Cell<u32>,
    /// Thumbnails rendered so far (for diagnostics).
    #[cfg_attr(not(feature = "devshot"), allow(dead_code))]
    pub renders: Cell<u32>,
}

impl Thumbs {
    pub fn new(selectable: bool) -> Rc<Thumbs> {
        let model = gtk::StringList::new(&[]);
        let (selection_model, selection): (gtk::SelectionModel, Option<gtk::MultiSelection>) = if selectable {
            let m = gtk::MultiSelection::new(Some(model.clone()));
            (m.clone().upcast(), Some(m))
        } else {
            (gtk::NoSelection::new(Some(model.clone())).upcast(), None)
        };
        let factory = gtk::SignalListItemFactory::new();
        let view = gtk::GridView::new(Some(selection_model), Some(factory.clone()));
        view.set_min_columns(1);
        view.set_max_columns(64);
        view.add_css_class("thumbs");
        view.set_enable_rubberband(selectable);
        if !selectable {
            view.set_can_focus(false);
        }
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .hexpand(true)
            .build();

        let thumbs = Rc::new(Thumbs {
            scrolled,
            view,
            model,
            selection,
            size: Cell::new(210),
            cache: RefCell::new(HashMap::new()),
            renderer: RefCell::new(None),
            sizer: RefCell::new(None),
            bound: RefCell::new(Vec::new()),
            pending: RefCell::new(Vec::new()),
            render_scheduled: Cell::new(false),
            quiet_ticks: Cell::new(0),
            renders: Cell::new(0),
        });

        let weak = Rc::downgrade(&thumbs);
        factory.connect_setup(move |_, obj| {
            let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
            let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
            let (frame, _) = super::sized_picture();
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&frame));
            overlay.set_halign(gtk::Align::Center);
            overlay.set_valign(gtk::Align::End);
            overlay.set_vexpand(true);
            if selectable {
                overlay.add_overlay(&check_box(item, weak.clone()));
            }
            // Caption: an optional badge ("Front 2") and the text ("pp. 3–4").
            let caption = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            caption.set_halign(gtk::Align::Center);
            let badge = gtk::Label::new(None);
            badge.add_css_class("sheet-badge");
            badge.set_visible(false);
            let label = gtk::Label::new(None);
            label.add_css_class("caption");
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            caption.append(&badge);
            caption.append(&label);
            card.add_css_class("thumb-card");
            card.append(&overlay);
            card.append(&caption);
            item.set_child(Some(&card));
        });
        let weak = Rc::downgrade(&thumbs);
        factory.connect_bind(move |_, obj| {
            if let Some(t) = weak.upgrade() {
                t.bind(obj.downcast_ref::<gtk::ListItem>().unwrap());
            }
        });
        let weak = Rc::downgrade(&thumbs);
        factory.connect_unbind(move |_, obj| {
            if let Some(t) = weak.upgrade() {
                let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
                t.bound.borrow_mut().retain(|c| c.item.upgrade().as_ref() != Some(item));
            }
        });

        // Anything that changes what's on screen may need new renders.
        let adj = thumbs.scrolled.vadjustment();
        let weak = Rc::downgrade(&thumbs);
        adj.connect_value_changed(move |_| schedule(&weak));
        let weak = Rc::downgrade(&thumbs);
        adj.connect_changed(move |_| schedule(&weak));
        let weak = Rc::downgrade(&thumbs);
        thumbs.scrolled.connect_map(move |_| schedule(&weak));
        thumbs
    }

    fn logical_size(&self, pos: u32) -> (i32, i32) {
        let (w, h) = self.sizer.borrow().as_ref().map(|s| s(pos)).unwrap_or((612.0, 792.0));
        let k = self.size.get() as f64 / w.max(h).max(1.0);
        ((w * k).round().max(1.0) as i32, (h * k).round().max(1.0) as i32)
    }

    fn bind(self: &Rc<Self>, item: &gtk::ListItem) {
        let pos = item.position();
        let card = item.child().and_downcast::<gtk::Box>().unwrap();
        let overlay = card.first_child().and_downcast::<gtk::Overlay>().unwrap();
        let frame = overlay.child().and_downcast::<adw::Clamp>().unwrap();
        let picture = frame.child().and_downcast::<adw::Clamp>().and_then(|v| v.child()).and_downcast::<gtk::Picture>().unwrap();
        let caption = overlay.next_sibling().unwrap();
        let badge = caption.first_child().and_downcast::<gtk::Label>().unwrap();
        let label = badge.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let text = self.model.string(pos).unwrap_or_default();
        match text.split_once('\t') {
            Some((b, rest)) => {
                badge.set_text(b);
                badge.set_visible(true);
                for (class, prefix) in [("front", "Front"), ("back", "Back")] {
                    if b.starts_with(prefix) {
                        badge.add_css_class(class);
                    } else {
                        badge.remove_css_class(class);
                    }
                }
                label.set_text(rest);
            }
            None => {
                badge.set_visible(false);
                label.set_text(&text);
            }
        }
        let entry = Card { pos, item: item.downgrade(), card: card.downgrade(), picture: picture.downgrade() };
        {
            let mut bound = self.bound.borrow_mut();
            bound.retain(|c| c.item.upgrade().as_ref() != Some(item));
            bound.push(entry.clone());
        }
        self.show(&entry);
    }

    /// Size a card for its page and show the cached image, or queue a render.
    fn show(self: &Rc<Self>, entry: &Card) {
        let (Some(card), Some(picture)) = (entry.card.upgrade(), entry.picture.upgrade()) else { return };
        let (w, h) = self.logical_size(entry.pos);
        card.set_size_request(self.size.get(), -1);
        super::set_picture_size(&picture, w, h);
        if let Some(tex) = self.cache.borrow().get(&entry.pos) {
            picture.set_paintable(Some(tex));
            picture.remove_css_class("pending");
            return;
        }
        picture.set_paintable(None::<&gdk::Paintable>);
        picture.add_css_class("pending");
        self.pending.borrow_mut().push(entry.clone());
        schedule(&Rc::downgrade(self));
    }

    /// Is this card inside the visible part of the grid (or about to scroll into it)?
    fn on_screen(&self, picture: &gtk::Picture) -> bool {
        // Cards that haven't been laid out yet all sit at 0,0 and would look visible.
        if !picture.is_mapped() || picture.width() == 0 || self.scrolled.height() == 0 {
            return false;
        }
        let Some(bounds) = picture.compute_bounds(&self.scrolled) else { return false };
        let margin = self.size.get() as f32; // one row of look-ahead
        bounds.y() + bounds.height() >= -margin && bounds.y() <= self.scrolled.height() as f32 + margin
    }

    /// Render visible pending cards for up to one time slice.
    fn render_some(&self) -> Progress {
        let started = Instant::now();
        let px = self.size.get() * super::device_scale(&self.view);
        let mut progress = Progress { rendered: false, more: false, unsettled: false };
        let mut i = 0;
        loop {
            if started.elapsed() > SLICE {
                progress.more = true;
                return progress;
            }
            let job = {
                let mut pending = self.pending.borrow_mut();
                // Drop cards that were recycled for another page or destroyed.
                pending.retain(Card::is_current);
                if i >= pending.len() {
                    return progress;
                }
                let p = &pending[i];
                let picture = p.picture.upgrade().unwrap();
                if picture.is_mapped() && picture.width() == 0 {
                    progress.unsettled = true;
                }
                if !self.on_screen(&picture) {
                    i += 1;
                    continue;
                }
                let p = pending.remove(i);
                (p.pos, picture)
            };
            let (pos, picture) = job;
            let cached = self.cache.borrow().get(&pos).cloned();
            let tex = cached.or_else(|| {
                self.renders.set(self.renders.get() + 1);
                let tex = self.renderer.borrow().as_ref()?(pos, px)?;
                let mut cache = self.cache.borrow_mut();
                if cache.len() >= CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(pos, tex.clone());
                Some(tex)
            });
            if let Some(tex) = tex {
                picture.set_paintable(Some(&tex));
                picture.remove_css_class("pending");
            }
            progress.rendered = true;
        }
    }

    fn reset(&self) {
        self.cache.borrow_mut().clear();
        self.pending.borrow_mut().clear();
    }

    /// Replace the contents.
    pub fn set_items(&self, labels: &[String], renderer: Renderer, sizer: Sizer) {
        self.reset();
        self.renderer.replace(Some(renderer));
        self.sizer.replace(Some(sizer));
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.model.splice(0, self.model.n_items(), &labels);
    }

    pub fn clear(&self) {
        self.reset();
        self.renderer.replace(None);
        self.sizer.replace(None);
        self.model.splice(0, self.model.n_items(), &[]);
    }

    pub fn set_size(self: &Rc<Self>, px: i32) {
        let px = px.clamp(MIN_SIZE, MAX_SIZE);
        if px == self.size.get() {
            return;
        }
        self.size.set(px);
        self.reset();
        // Resize the cards in place; GridView won't rebind unchanged items.
        let cards: Vec<Card> = self.bound.borrow().iter().filter(|c| c.is_current()).cloned().collect();
        for card in &cards {
            self.show(card);
        }
    }

    /// Click the check box on page `pos`, if its card is on screen.
    #[cfg(feature = "devshot")]
    pub fn click_check(&self, pos: u32) -> bool {
        let card = self.bound.borrow().iter().find(|c| c.pos == pos && c.is_current()).cloned();
        let check = card
            .and_then(|c| c.card.upgrade())
            .and_then(|b| b.first_child())
            .and_then(|o| o.last_child())
            .and_downcast::<gtk::CheckButton>();
        match check {
            Some(c) => {
                c.set_active(!c.is_active());
                true
            }
            None => false,
        }
    }

    /// Selected positions (selectable grids only).
    pub fn selected(&self) -> Vec<usize> {
        let Some(sel) = &self.selection else { return Vec::new() };
        let set = sel.selection();
        (0..set.size()).map(|i| set.nth(i as u32) as usize).collect()
    }

    pub fn set_selected(&self, rows: &[usize]) {
        let Some(sel) = &self.selection else { return };
        let set = gtk::Bitset::new_empty();
        for &r in rows {
            set.add(r as u32);
        }
        sel.set_selection(&set, &gtk::Bitset::new_range(0, self.model.n_items()));
    }
}

/// Render visible cards soon, a slice at a time, without blocking the window.
/// Keeps checking for a few ticks after the last change, until layout settles.
fn schedule(weak: &Weak<Thumbs>) {
    let Some(t) = weak.upgrade() else { return };
    t.quiet_ticks.set(0);
    if t.render_scheduled.replace(true) {
        return;
    }
    let weak = weak.clone();
    glib::timeout_add_local(TICK, move || {
        let Some(t) = weak.upgrade() else { return glib::ControlFlow::Break };
        let progress = t.render_some();
        if progress.rendered || progress.more || progress.unsettled {
            t.quiet_ticks.set(0);
        } else {
            t.quiet_ticks.set(t.quiet_ticks.get() + 1);
        }
        if t.quiet_ticks.get() < SETTLE_TICKS {
            glib::ControlFlow::Continue
        } else {
            t.render_scheduled.set(false);
            glib::ControlFlow::Break
        }
    });
}

/// Round check box in the corner of a page, kept in step with the selection.
fn check_box(item: &gtk::ListItem, thumbs: Weak<Thumbs>) -> gtk::CheckButton {
    let check = gtk::CheckButton::new();
    check.add_css_class("selection-mode");
    check.add_css_class("page-check");
    check.set_halign(gtk::Align::End);
    check.set_valign(gtk::Align::Start);
    check.set_margin_top(6);
    check.set_margin_end(6);
    check.set_tooltip_text(Some("Include this page"));
    // Selection -> check box
    let weak_check = check.downgrade();
    item.connect_selected_notify(move |item| {
        if let Some(check) = weak_check.upgrade() {
            check.set_active(item.is_selected());
        }
    });
    // Check box -> selection (only this page; the others stay as they are)
    let weak_item = item.downgrade();
    check.connect_toggled(move |check| {
        let (Some(item), Some(t)) = (weak_item.upgrade(), thumbs.upgrade()) else { return };
        let Some(sel) = &t.selection else { return };
        if check.is_active() == item.is_selected() {
            return;
        }
        let pos = item.position();
        if check.is_active() {
            sel.select_item(pos, false);
        } else {
            sel.unselect_item(pos);
        }
    });
    check
}
