//! The Print Studio window: thumbnails on the left, options on the right.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib::clone;
use gtk::{gdk, gio, glib};

use super::thumbs::{MAX_SIZE, MIN_SIZE, Thumbs};
use super::{apply_color_scheme, fit_to_monitor, viewer};
use printstudio::config::{Config, PpdValues, merge_job};
use printstudio::cups::{self, PrinterInfo};
use printstudio::pipeline::{self, JobOptions, NUP_CHOICES, Orientation, PageSet, Pass, Scaling, Side};
use printstudio::geometry::SheetLayout;
use printstudio::render::{SheetSpec, Source, displayed_sheet_size, render_scaled, render_sheet};
use printstudio::spool::SpoolJob;
use printstudio::{convert, pdfout, testpdf};

const ZOOM_STEP: f64 = 1.2;

thread_local! {
    /// Open windows. Callbacks only hold weak references, so this is what keeps
    /// each window's state alive until the window closes.
    static OPEN: RefCell<Vec<Rc<Win>>> = const { RefCell::new(Vec::new()) };
}
const PAGE_SETS: [(&str, PageSet); 3] = [("All pages", PageSet::All), ("Odd pages only", PageSet::Odd), ("Even pages only", PageSet::Even)];
const ROTATIONS: [(&str, u32); 4] = [("None", 0), ("90° clockwise", 90), ("180°", 180), ("90° counter-clockwise", 270)];
const ORIENTATIONS: [(&str, Orientation); 3] =
    [("Automatic", Orientation::Auto), ("Portrait", Orientation::Portrait), ("Landscape", Orientation::Landscape)];
const SCALINGS: [(&str, Scaling); 3] =
    [("Fit to printable area", Scaling::Fit), ("Shrink only if too big", Scaling::Shrink), ("Actual size", Scaling::Actual)];
const INSTRUCTIONS: &str = "\
1. Wait until <b>all front sides</b> have finished printing.
2. Take the whole stack out of the output tray. <b>Don't reorder it.</b>
3. Put it back in the paper tray so the <b>blank sides</b> get printed next.
4. Click <b>Print back sides</b>.

First time with this printer? Use <i>Print calibration test</i> to check how the stack has to go back in.";

struct Doc {
    path: PathBuf,
    title: String,
    source: Rc<Source>,
}

/// What the preview currently shows (and what Print will print).
#[derive(Clone)]
struct Output {
    job: JobOptions,
    passes: Vec<Pass>,
    sheets: Vec<(Side, u32)>,
    labels: Vec<String>,
    layout: SheetLayout,
    fallback: (f64, f64),
}

pub struct Win {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    stack: adw::ViewStack,
    preview: Rc<Thumbs>,
    pages: Rc<Thumbs>,
    selection_label: gtk::Label,
    summary: gtk::Label,
    zoom: gtk::Scale,

    printer_row: adw::ComboRow,
    preset_row: adw::ComboRow,
    range_row: adw::EntryRow,
    page_set_row: adw::ComboRow,
    copies_row: adw::SpinRow,
    collate_row: adw::SwitchRow,
    reverse_row: adw::SwitchRow,
    nup_row: adw::ComboRow,
    rotate_row: adw::ComboRow,
    orientation_row: adw::ComboRow,
    scaling_row: adw::ComboRow,
    duplex_row: adw::ExpanderRow,
    backs_reverse_row: adw::SwitchRow,
    backs_rotate_row: adw::SwitchRow,
    note_row: adw::EntryRow,
    prefs: adw::PreferencesPage,
    ppd_groups: RefCell<Vec<adw::PreferencesGroup>>,
    ppd_rows: RefCell<Vec<(String, adw::ComboRow, Vec<String>)>>,

    status: gtk::Label,
    spinner: gtk::Spinner,
    print_button: gtk::Button,

    cfg: RefCell<Config>,
    printers: RefCell<Vec<String>>,
    presets: RefCell<Vec<String>>,
    info: RefCell<PrinterInfo>,
    doc: RefCell<Option<Doc>>,
    output: RefCell<Option<Output>>,
    spool_job: Option<SpoolJob>,
    tmp: PathBuf,
    applying: Cell<bool>,
    syncing: Cell<bool>,
    busy: Cell<bool>,
    rebuild_source: RefCell<Option<glib::SourceId>>,
}

fn combo(title: &str, items: &[&str]) -> adw::ComboRow {
    let row = adw::ComboRow::new();
    row.set_title(title);
    row.set_model(Some(&gtk::StringList::new(items)));
    row
}

fn switch(title: &str, subtitle: &str) -> adw::SwitchRow {
    let row = adw::SwitchRow::new();
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

fn set_combo_items(row: &adw::ComboRow, items: &[String]) {
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk::StringList::new(&items)));
}

fn temp_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("printstudio-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

impl Win {
    pub fn new(app: &adw::Application, file: Option<PathBuf>, spool_job: Option<SpoolJob>) -> Rc<Win> {
        let cfg = Config::load();
        apply_color_scheme(cfg.ui("color_scheme").and_then(|v| v.as_str()).unwrap_or("system"));

        let window = adw::ApplicationWindow::new(app);
        window.set_title(Some("Print Studio"));
        fit_to_monitor(window.upcast_ref(), None, 1180, 800, 0.85);

        // Header bar
        let title = adw::WindowTitle::new("Print Studio", "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let open_button = gtk::Button::from_icon_name("document-open-symbolic");
        open_button.set_tooltip_text(Some("Open a document (Ctrl+O)"));
        open_button.set_action_name(Some("win.open"));
        header.pack_start(&open_button);
        let menu = gio::Menu::new();
        let appearance = gio::Menu::new();
        appearance.append(Some("Follow system"), Some("win.color-scheme::system"));
        appearance.append(Some("Light"), Some("win.color-scheme::light"));
        appearance.append(Some("Dark"), Some("win.color-scheme::dark"));
        menu.append_section(Some("Appearance"), &appearance);
        let menu_button = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Menu").build();
        header.pack_end(&menu_button);

        // Left: preview / page selection
        let preview = Thumbs::new(false);
        preview.view.set_tooltip_text(Some("Exactly what will print, in print order. Double-click to zoom in."));
        let pages = Thumbs::new(true);
        pages.view.set_tooltip_text(Some("Tick the pages to print (or Ctrl+click, Shift+click, drag). Double-click to zoom in."));

        let selection_label = gtk::Label::new(None);
        selection_label.set_hexpand(true);
        selection_label.set_xalign(0.0);
        let sel_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        sel_bar.set_margin_start(12);
        sel_bar.set_margin_end(12);
        sel_bar.set_margin_top(6);
        sel_bar.set_margin_bottom(6);
        sel_bar.append(&selection_label);
        let select_all = gtk::Button::with_label("Select all");
        let clear = gtk::Button::with_label("Clear");
        clear.set_tooltip_text(Some("No selection means every page prints"));
        let invert = gtk::Button::with_label("Invert");
        for b in [&select_all, &clear, &invert] {
            b.add_css_class("flat");
            sel_bar.append(b);
        }
        let pages_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pages_box.append(&sel_bar);
        pages_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        pages_box.append(&pages.scrolled);

        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(&preview.scrolled, Some("preview"), "Print preview", "printer-symbolic");
        stack.add_titled_with_icon(&pages_box, Some("pages"), "Select pages", "view-grid-symbolic");
        stack.set_vexpand(true);
        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        let switcher_bar = gtk::CenterBox::new();
        switcher_bar.set_center_widget(Some(&switcher));
        switcher_bar.set_margin_top(6);
        switcher_bar.set_margin_bottom(6);

        let summary = gtk::Label::new(None);
        summary.set_hexpand(true);
        summary.set_xalign(0.0);
        summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        summary.add_css_class("dim-label");
        let zoom_out = gtk::Button::from_icon_name("zoom-out-symbolic");
        zoom_out.set_tooltip_text(Some("Smaller thumbnails (Ctrl+−, or Ctrl+scroll)"));
        zoom_out.set_action_name(Some("win.zoom-out"));
        zoom_out.add_css_class("flat");
        let zoom = gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_SIZE as f64, MAX_SIZE as f64, 10.0);
        zoom.set_draw_value(false);
        zoom.set_tooltip_text(Some("Thumbnail size"));
        let zoom_in = gtk::Button::from_icon_name("zoom-in-symbolic");
        zoom_in.set_tooltip_text(Some("Bigger thumbnails (Ctrl++, or Ctrl+scroll)"));
        zoom_in.set_action_name(Some("win.zoom-in"));
        zoom_in.add_css_class("flat");
        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        bottom.add_css_class("zoom-bar");
        bottom.set_margin_start(12);
        bottom.set_margin_end(8);
        bottom.set_margin_top(4);
        bottom.set_margin_bottom(4);
        bottom.append(&summary);
        bottom.append(&zoom_out);
        bottom.append(&zoom);
        bottom.append(&zoom_in);

        let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
        left.append(&switcher_bar);
        left.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        left.append(&stack);
        left.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        left.append(&bottom);
        left.add_css_class("view");

        // Right: options
        let prefs = adw::PreferencesPage::new();

        let printer_group = adw::PreferencesGroup::new();
        let printer_row = combo("Printer", &[]);
        printer_group.add(&printer_row);
        let preset_row = combo("Preset", &[]);
        let delete_preset = gtk::Button::from_icon_name("user-trash-symbolic");
        delete_preset.set_tooltip_text(Some("Delete this preset"));
        delete_preset.add_css_class("flat");
        delete_preset.set_valign(gtk::Align::Center);
        let save_preset = gtk::Button::from_icon_name("document-save-symbolic");
        save_preset.set_tooltip_text(Some("Save the current settings as a preset"));
        save_preset.add_css_class("flat");
        save_preset.set_valign(gtk::Align::Center);
        preset_row.add_suffix(&save_preset);
        preset_row.add_suffix(&delete_preset);
        printer_group.add(&preset_row);
        prefs.add(&printer_group);

        let pages_group = adw::PreferencesGroup::new();
        pages_group.set_title("Pages");
        let range_row = adw::EntryRow::new();
        range_row.set_title("Pages (e.g. 1-3, 7, 10-)");
        let page_set_row = combo("Print", &PAGE_SETS.map(|(l, _)| l));
        let copies_row = adw::SpinRow::with_range(1.0, 999.0, 1.0);
        copies_row.set_title("Copies");
        let collate_row = switch("Collate", "1, 2, 3, 1, 2, 3 instead of 1, 1, 2, 2, 3, 3");
        let reverse_row = switch("Reverse order", "Last page first, for printers that stack pages face up");
        let nup_labels: Vec<String> = NUP_CHOICES.iter().map(|n| n.to_string()).collect();
        let nup_row = combo("Pages per sheet", &nup_labels.iter().map(String::as_str).collect::<Vec<_>>());
        let rotate_row = combo("Rotate", &ROTATIONS.map(|(l, _)| l));
        let orientation_row = combo("Orientation", &ORIENTATIONS.map(|(l, _)| l));
        orientation_row.set_subtitle("Automatic turns the paper to match each page");
        let scaling_row = combo("Scaling", &SCALINGS.map(|(l, _)| l));
        for row in [range_row.upcast_ref::<gtk::Widget>(), page_set_row.upcast_ref(), copies_row.upcast_ref(),
                    collate_row.upcast_ref(), reverse_row.upcast_ref(), nup_row.upcast_ref(), orientation_row.upcast_ref(),
                    scaling_row.upcast_ref(), rotate_row.upcast_ref()] {
            pages_group.add(row);
        }
        prefs.add(&pages_group);

        let duplex_group = adw::PreferencesGroup::new();
        let duplex_row = adw::ExpanderRow::new();
        duplex_row.set_title("Manual two-sided printing");
        duplex_row.set_subtitle("Prints the fronts, then walks you through reloading the stack for the backs");
        duplex_row.set_show_enable_switch(true);
        duplex_row.set_enable_expansion(false);
        let backs_reverse_row = switch("Reverse order of back sides", "");
        let backs_rotate_row = switch("Rotate back sides 180°", "");
        let note_row = adw::EntryRow::new();
        note_row.set_title("Your reloading note, e.g. “printed side up, top edge first”");
        let calibration_row = adw::ActionRow::new();
        calibration_row.set_title("Calibration test");
        calibration_row.set_subtitle("Prints 2 sheets that show whether the settings above are right");
        let calibration_button = gtk::Button::with_label("Print test");
        calibration_button.set_valign(gtk::Align::Center);
        calibration_row.add_suffix(&calibration_button);
        duplex_row.add_row(&backs_reverse_row);
        duplex_row.add_row(&backs_rotate_row);
        duplex_row.add_row(&note_row);
        duplex_row.add_row(&calibration_row);
        duplex_group.add(&duplex_row);
        prefs.add(&duplex_group);

        let status = gtk::Label::new(None);
        status.set_wrap(true);
        status.set_xalign(0.0);
        status.set_margin_start(12);
        status.set_margin_end(12);
        status.set_visible(false);
        let spinner = gtk::Spinner::new();
        let save_pdf = gtk::Button::with_label("Save as PDF…");
        let cancel = gtk::Button::with_label("Cancel");
        cancel.set_action_name(Some("window.close"));
        let print_button = gtk::Button::with_label("Print");
        print_button.add_css_class("suggested-action");
        print_button.set_action_name(Some("win.print"));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_margin_start(12);
        actions.set_margin_end(12);
        actions.set_margin_top(8);
        actions.set_margin_bottom(12);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions.append(&save_pdf);
        actions.append(&spacer);
        actions.append(&spinner);
        actions.append(&cancel);
        actions.append(&print_button);

        let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
        right.set_size_request(400, -1);
        prefs.set_vexpand(true);
        right.append(&prefs);
        right.append(&status);
        right.append(&actions);

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&left));
        paned.set_end_child(Some(&right));
        paned.set_resize_end_child(false);
        paned.set_shrink_end_child(false);
        paned.set_shrink_start_child(false);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&paned));
        window.set_content(Some(&toolbar));

        let win = Rc::new(Win {
            window,
            title,
            stack,
            preview,
            pages,
            selection_label,
            summary,
            zoom,
            printer_row,
            preset_row,
            range_row,
            page_set_row,
            copies_row,
            collate_row,
            reverse_row,
            nup_row,
            rotate_row,
            orientation_row,
            scaling_row,
            duplex_row,
            backs_reverse_row,
            backs_rotate_row,
            note_row,
            prefs,
            ppd_groups: RefCell::new(Vec::new()),
            ppd_rows: RefCell::new(Vec::new()),
            status,
            spinner,
            print_button,
            cfg: RefCell::new(cfg),
            printers: RefCell::new(Vec::new()),
            presets: RefCell::new(Vec::new()),
            info: RefCell::new(PrinterInfo::default()),
            doc: RefCell::new(None),
            output: RefCell::new(None),
            spool_job,
            tmp: temp_dir(),
            applying: Cell::new(false),
            syncing: Cell::new(false),
            busy: Cell::new(false),
            rebuild_source: RefCell::new(None),
        });

        win.setup_actions();
        win.connect_signals(&select_all, &clear, &invert, &save_preset, &delete_preset, &calibration_button, &save_pdf);
        win.restore_ui_prefs();
        win.load_printers();
        win.refresh_presets(None);

        if let Some(job) = win.spool_job.clone() {
            win.copies_row.set_value(job.copies as f64);
            let title = if job.title.is_empty() { "Untitled".to_string() } else { job.title.clone() };
            win.open_pdf(job.pdf.clone(), title);
        } else if let Some(path) = file {
            win.open_path(path);
        }
        win.schedule();
        OPEN.with(|open| open.borrow_mut().push(win.clone()));
        win
    }

    pub fn present(&self) {
        self.window.present();
    }

    // Setup

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<Win>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&action);
        };
        add("print", |w| w.print());
        add("open", |w| w.open_dialog());
        add("zoom-in", |w| w.zoom_step(1));
        add("zoom-out", |w| w.zoom_step(-1));
        add("escape", |w| {
            // An open dialog takes Escape; otherwise it closes the window.
            match w.window.visible_dialog() {
                Some(dialog) => {
                    dialog.close();
                }
                None => w.window.close(),
            }
        });

        let scheme = self.cfg.borrow().ui("color_scheme").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "system".into());
        let action = gio::SimpleAction::new_stateful("color-scheme", Some(glib::VariantTy::STRING), &scheme.to_variant());
        let weak = Rc::downgrade(self);
        action.connect_activate(move |a, param| {
            let (Some(w), Some(value)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else { return };
            a.set_state(&value.to_variant());
            apply_color_scheme(&value);
            w.update_config(|c| c.set_ui("color_scheme", value.clone().into()));
        });
        self.window.add_action(&action);
    }

    #[allow(clippy::too_many_arguments)]
    fn connect_signals(self: &Rc<Self>, select_all: &gtk::Button, clear: &gtk::Button, invert: &gtk::Button,
                       save_preset: &gtk::Button, delete_preset: &gtk::Button, calibration: &gtk::Button, save_pdf: &gtk::Button) {
        let w = self;
        for row in [&w.page_set_row, &w.nup_row, &w.rotate_row, &w.orientation_row, &w.scaling_row] {
            row.connect_selected_notify(clone!(#[weak] w, move |_| w.schedule()));
        }
        for row in [&w.collate_row, &w.reverse_row, &w.backs_reverse_row, &w.backs_rotate_row] {
            row.connect_active_notify(clone!(#[weak] w, move |_| w.schedule()));
        }
        w.copies_row.connect_value_notify(clone!(#[weak] w, move |_| w.schedule()));
        w.duplex_row.connect_enable_expansion_notify(clone!(#[weak] w, move |_| w.schedule()));
        w.range_row.connect_changed(clone!(#[weak] w, move |row| {
            w.schedule();
            if !w.syncing.get() {
                w.select_from_ranges(&row.text());
            }
        }));
        w.printer_row.connect_selected_notify(clone!(#[weak] w, move |_| {
            if !w.applying.get() {
                w.on_printer_changed();
            }
        }));
        w.preset_row.connect_selected_notify(clone!(#[weak] w, move |_| {
            if !w.applying.get() {
                w.apply_preset();
            }
        }));
        save_preset.connect_clicked(clone!(#[weak] w, move |_| w.save_preset()));
        delete_preset.connect_clicked(clone!(#[weak] w, move |_| w.delete_preset()));
        calibration.connect_clicked(clone!(#[weak] w, move |_| w.print_calibration()));
        save_pdf.connect_clicked(clone!(#[weak] w, move |_| w.save_pdf()));

        select_all.connect_clicked(clone!(#[weak] w, move |_| {
            if let Some(s) = &w.pages.selection {
                s.select_all();
            }
        }));
        clear.connect_clicked(clone!(#[weak] w, move |_| {
            if let Some(s) = &w.pages.selection {
                s.unselect_all();
            }
        }));
        invert.connect_clicked(clone!(#[weak] w, move |_| {
            let selected = w.pages.selected();
            let rows: Vec<usize> = (0..w.pages.model.n_items() as usize).filter(|r| !selected.contains(r)).collect();
            w.pages.set_selected(&rows);
        }));
        if let Some(sel) = &w.pages.selection {
            sel.connect_selection_changed(clone!(#[weak] w, move |_, _, _| w.on_page_selection()));
        }

        w.preview.view.connect_activate(clone!(#[weak] w, move |_, pos| w.view_preview(pos)));
        w.pages.view.connect_activate(clone!(#[weak] w, move |_, pos| w.view_source(pos)));
        w.zoom.connect_value_changed(clone!(#[weak] w, move |s| {
            w.preview.set_size(s.value() as i32);
            w.pages.set_size(s.value() as i32);
        }));
        for scrolled in [&w.preview.scrolled, &w.pages.scrolled] {
            let ctl = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
            ctl.connect_scroll(clone!(#[weak] w, #[upgrade_or] glib::Propagation::Proceed, move |ctl, _, dy| {
                if ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    w.zoom_step(if dy < 0.0 { 1 } else { -1 });
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }));
            scrolled.add_controller(ctl);
        }

        let drop = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(clone!(#[weak] w, #[upgrade_or] false, move |_, value, _, _| {
            match value.get::<gio::File>().ok().and_then(|f| f.path()) {
                Some(path) => {
                    w.open_path(path);
                    true
                }
                None => false,
            }
        }));
        w.window.add_controller(drop);

        w.window.connect_close_request(clone!(#[weak] w, #[upgrade_or] glib::Propagation::Proceed, move |_| {
            w.on_close();
            glib::Propagation::Proceed
        }));
    }

    fn restore_ui_prefs(&self) {
        let cfg = self.cfg.borrow();
        let size = cfg.ui("thumb_size").and_then(|v| v.as_i64()).unwrap_or(210) as i32;
        self.zoom.set_value(size as f64);
        self.preview.set_size(size);
        self.pages.set_size(size);
        if cfg.ui("tab").and_then(|v| v.as_str()) == Some("pages") {
            self.stack.set_visible_child_name("pages");
        }
    }

    fn on_close(&self) {
        let size = self.zoom.value() as i64;
        let tab = if self.stack.visible_child_name().as_deref() == Some("pages") { "pages" } else { "preview" };
        self.update_config(|c| {
            c.set_ui("thumb_size", size.into());
            c.set_ui("tab", tab.into());
        });
        if let Some(job) = &self.spool_job {
            job.remove();
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
        OPEN.with(|open| open.borrow_mut().retain(|w| !std::ptr::eq(w.as_ref(), self)));
    }

    /// Change the config on disk. Re-read first: another Print Studio window may have saved since.
    fn update_config(&self, f: impl FnOnce(&mut Config)) {
        let path = self.cfg.borrow().path.clone();
        let mut fresh = Config::load_from(path);
        f(&mut fresh);
        if let Err(e) = fresh.save() {
            eprintln!("Couldn't save settings: {e}");
        }
        self.cfg.replace(fresh);
    }

    // Reading and applying settings

    fn read_job(&self) -> JobOptions {
        JobOptions {
            page_ranges: self.range_row.text().to_string(),
            page_set: PAGE_SETS.get(self.page_set_row.selected() as usize).map(|p| p.1).unwrap_or_default(),
            copies: self.copies_row.value().round().max(1.0) as u32,
            collate: self.collate_row.is_active(),
            reverse: self.reverse_row.is_active(),
            nup: NUP_CHOICES.get(self.nup_row.selected() as usize).copied().unwrap_or(1),
            rotate: ROTATIONS.get(self.rotate_row.selected() as usize).map(|r| r.1).unwrap_or(0),
            orientation: ORIENTATIONS.get(self.orientation_row.selected() as usize).map(|o| o.1).unwrap_or_default(),
            scaling: SCALINGS.get(self.scaling_row.selected() as usize).map(|s| s.1).unwrap_or_default(),
            duplex: self.duplex_row.enables_expansion(),
            backs_reverse: self.backs_reverse_row.is_active(),
            backs_rotate: self.backs_rotate_row.is_active(),
        }
    }

    fn apply_job(&self, job: &JobOptions) {
        self.applying.set(true);
        self.range_row.set_text(&job.page_ranges);
        self.page_set_row.set_selected(PAGE_SETS.iter().position(|p| p.1 == job.page_set).unwrap_or(0) as u32);
        self.copies_row.set_value(job.copies.max(1) as f64);
        self.collate_row.set_active(job.collate);
        self.reverse_row.set_active(job.reverse);
        self.nup_row.set_selected(NUP_CHOICES.iter().position(|n| *n == job.nup).unwrap_or(0) as u32);
        self.rotate_row.set_selected(ROTATIONS.iter().position(|r| r.1 == job.rotate).unwrap_or(0) as u32);
        self.orientation_row.set_selected(ORIENTATIONS.iter().position(|o| o.1 == job.orientation).unwrap_or(0) as u32);
        self.scaling_row.set_selected(SCALINGS.iter().position(|s| s.1 == job.scaling).unwrap_or(0) as u32);
        self.duplex_row.set_enable_expansion(job.duplex);
        self.backs_reverse_row.set_active(job.backs_reverse);
        self.backs_rotate_row.set_active(job.backs_rotate);
        self.applying.set(false);
    }

    fn read_ppd(&self) -> PpdValues {
        self.ppd_rows
            .borrow()
            .iter()
            .filter_map(|(kw, row, values)| Some((kw.clone(), values.get(row.selected() as usize)?.clone())))
            .collect()
    }

    fn apply_ppd(&self, values: &PpdValues) {
        self.applying.set(true);
        for (kw, row, choices) in self.ppd_rows.borrow().iter() {
            if let Some(i) = values.get(kw).and_then(|v| choices.iter().position(|c| c == v)) {
                row.set_selected(i as u32);
            }
        }
        self.applying.set(false);
    }

    fn paper(&self) -> Option<(f64, f64)> {
        let values = self.read_ppd();
        self.info.borrow().paper(values.get("PageSize").map(String::as_str))
    }

    /// Unprintable border for the chosen paper, from the printer driver.
    fn margin(&self) -> f64 {
        let values = self.read_ppd();
        self.info.borrow().margin(values.get("PageSize").map(String::as_str))
    }

    // Printers and presets

    fn load_printers(self: &Rc<Self>) {
        let names = cups::printers();
        set_combo_items(&self.printer_row, &names);
        let last = self.cfg.borrow().last_printer().filter(|p| names.contains(p));
        let preferred = last.or_else(|| cups::default_printer(&names));
        self.printers.replace(names.clone());
        match preferred.and_then(|p| names.iter().position(|n| *n == p)) {
            Some(i) => {
                self.applying.set(true);
                self.printer_row.set_selected(i as u32);
                self.applying.set(false);
                self.on_printer_changed();
            }
            None => self.set_status("No printers found. Add one in your system's printer settings.", true),
        }
    }

    fn printer_name(&self) -> Option<String> {
        self.printers.borrow().get(self.printer_row.selected() as usize).cloned()
    }

    fn on_printer_changed(self: &Rc<Self>) {
        let Some(name) = self.printer_name() else { return };
        let current = self.read_job();
        let info = cups::printer_info(&name);
        self.build_ppd_groups(&info);
        self.info.replace(info);
        let (mut job, ppd) = self.cfg.borrow().printer_settings(&name);
        job.page_ranges = current.page_ranges;
        job.copies = current.copies;
        self.apply_job(&job);
        self.apply_ppd(&ppd);
        self.note_row.set_text(&self.cfg.borrow().printer_note(&name));
        self.schedule();
    }

    fn build_ppd_groups(self: &Rc<Self>, info: &PrinterInfo) {
        for group in self.ppd_groups.take() {
            self.prefs.remove(&group);
        }
        let mut rows = Vec::new();
        let mut groups = Vec::new();
        let mut make_row = |opt: &printstudio::ppd::PrinterOption| {
            let labels: Vec<&str> = opt.choices.iter().map(|(_, l)| l.as_str()).collect();
            let row = combo(&opt.label, &labels);
            let values: Vec<String> = opt.choices.iter().map(|(v, _)| v.clone()).collect();
            row.set_selected(values.iter().position(|v| *v == opt.default).unwrap_or(0) as u32);
            row.connect_selected_notify(clone!(#[weak(rename_to = w)] self, move |_| w.schedule()));
            rows.push((opt.keyword.clone(), row.clone(), values));
            row
        };

        let common = adw::PreferencesGroup::new();
        common.set_title("Printer settings");
        for opt in info.options.iter().filter(|o| o.common) {
            common.add(&make_row(opt));
        }
        if info.options.is_empty() {
            common.set_description(Some("This printer doesn't list any options."));
        }
        groups.push(common);

        let advanced: Vec<_> = info.options.iter().filter(|o| !o.common).collect();
        if !advanced.is_empty() {
            let group = adw::PreferencesGroup::new();
            group.set_title("Advanced printer settings");
            let mut expander: Option<(String, adw::ExpanderRow)> = None;
            for opt in advanced {
                if expander.as_ref().map(|(g, _)| g != &opt.group).unwrap_or(true) {
                    let row = adw::ExpanderRow::new();
                    row.set_title(&opt.group);
                    group.add(&row);
                    expander = Some((opt.group.clone(), row));
                }
                expander.as_ref().unwrap().1.add_row(&make_row(opt));
            }
            groups.push(group);
        }
        for group in &groups {
            self.prefs.add(group);
        }
        self.ppd_groups.replace(groups);
        self.ppd_rows.replace(rows);
    }

    fn refresh_presets(&self, select: Option<&str>) {
        let names = self.cfg.borrow().preset_names();
        let mut items = vec!["None".to_string()];
        items.extend(names.iter().cloned());
        self.applying.set(true);
        set_combo_items(&self.preset_row, &items);
        let index = select.and_then(|s| names.iter().position(|n| n == s)).map(|i| i + 1).unwrap_or(0);
        self.preset_row.set_selected(index as u32);
        self.applying.set(false);
        self.presets.replace(names);
    }

    fn selected_preset(&self) -> Option<String> {
        let i = self.preset_row.selected() as usize;
        if i == 0 { None } else { self.presets.borrow().get(i - 1).cloned() }
    }

    fn apply_preset(self: &Rc<Self>) {
        let Some(name) = self.selected_preset() else { return };
        let (fields, ppd) = self.cfg.borrow().preset(&name);
        let job = merge_job(&self.read_job(), &fields);
        self.apply_job(&job);
        self.apply_ppd(&ppd);
        self.set_status(&format!("Preset “{name}” applied."), false);
        self.schedule();
    }

    fn save_preset(self: &Rc<Self>) {
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("e.g. Draft, grayscale, fast"));
        entry.set_text(&self.selected_preset().unwrap_or_default());
        entry.set_activates_default(true);
        let dialog = adw::AlertDialog::new(Some("Save preset"), Some("Saves the current page and printer settings under a name."));
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            if dialog.choose_future(Some(&w.window)).await != "save" {
                return;
            }
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            let (job, ppd) = (w.read_job(), w.read_ppd());
            w.update_config(|c| c.set_preset(&name, &job, &ppd));
            w.refresh_presets(Some(&name));
            w.set_status(&format!("Preset “{name}” saved."), false);
        }));
    }

    fn delete_preset(self: &Rc<Self>) {
        let Some(name) = self.selected_preset() else { return };
        let dialog = adw::AlertDialog::new(Some("Delete preset?"), Some(&format!("“{name}” will be removed.")));
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            if dialog.choose_future(Some(&w.window)).await == "delete" {
                w.update_config(|c| c.delete_preset(&name));
                w.refresh_presets(None);
            }
        }));
    }

    fn remember_printer_settings(&self) {
        let Some(name) = self.printer_name() else { return };
        let (job, ppd, note) = (self.read_job(), self.read_ppd(), self.note_row.text().trim().to_string());
        self.update_config(|c| {
            c.set_last_printer(&name);
            c.set_printer_settings(&name, &job, &ppd);
            c.set_printer_note(&name, &note);
        });
    }

    // Documents

    fn open_dialog(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Documents and images"));
        for pattern in ["*.pdf", "*.png", "*.jpg", "*.jpeg", "*.bmp", "*.gif", "*.tif", "*.tiff", "*.webp", "*.odt", "*.ods",
                        "*.odp", "*.doc", "*.docx", "*.xls", "*.xlsx", "*.ppt", "*.pptx", "*.rtf", "*.txt"] {
            filter.add_suffix(pattern.trim_start_matches("*."));
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title("Open document").filters(&filters).default_filter(&filter).build();
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            if let Ok(file) = dialog.open_future(Some(&w.window)).await
                && let Some(path) = file.path()
            {
                w.open_path(path);
            }
        }));
    }

    fn open_path(self: &Rc<Self>, path: PathBuf) {
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if convert::is_pdf(&path) {
            self.open_pdf(path, title);
            return;
        }
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            w.set_busy(true, "Converting to PDF…");
            let tmp = w.tmp.clone();
            let result = gio::spawn_blocking(move || convert::to_pdf(&path, &tmp)).await.unwrap_or_else(|_| Err("Conversion failed".into()));
            w.set_busy(false, "");
            match result {
                Ok(pdf) => w.open_pdf(pdf, title),
                Err(e) => w.error("Can't open file", &e),
            }
        }));
    }

    fn open_pdf(self: &Rc<Self>, path: PathBuf, title: String) {
        let source = match Source::open(&path) {
            Ok(s) => Rc::new(s),
            Err(e) => return self.error("Can't open file", &format!("{title}: {e}")),
        };
        let n = source.n_pages();
        self.title.set_title(&title);
        self.title.set_subtitle(&format!("{n} page{}", if n == 1 { "" } else { "s" }));
        let labels: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        let (s1, s2) = (source.clone(), source.clone());
        self.pages.set_items(
            &labels,
            Box::new(move |pos, px| {
                let side = vec![pos as usize];
                render_sheet(&s1, &SheetSpec::page(&side), px)
            }),
            Box::new(move |pos| s2.sizes[pos as usize]),
        );
        self.doc.replace(Some(Doc { path, title, source }));
        self.select_from_ranges(&self.range_row.text());
        self.schedule();
    }

    // Preview

    fn schedule(self: &Rc<Self>) {
        if self.applying.get() {
            return;
        }
        if let Some(id) = self.rebuild_source.take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(Duration::from_millis(120), clone!(#[weak(rename_to = w)] self, move || {
            w.rebuild_source.replace(None);
            w.rebuild();
        }));
        self.rebuild_source.replace(Some(id));
    }

    fn compute(&self) -> Result<Output, String> {
        let doc = self.doc.borrow();
        let doc = doc.as_ref().ok_or("No document")?;
        let job = self.read_job();
        let passes = pipeline::plan(doc.source.n_pages(), &job)?;
        let sheets = pipeline::flatten(&passes, &job);
        let labels = pipeline::side_labels(&passes, job.duplex);
        let first = sheets.iter().flat_map(|(s, _)| s.iter()).next().copied().unwrap_or(0);
        let fallback = doc.source.sizes.get(first).copied().unwrap_or((612.0, 792.0));
        let layout = SheetLayout::new(&job, self.paper(), self.margin());
        Ok(Output { job, passes, sheets, labels, layout, fallback })
    }

    fn rebuild(self: &Rc<Self>) {
        let source = self.doc.borrow().as_ref().map(|d| d.source.clone());
        let Some(source) = source else {
            self.preview.clear();
            self.summary.set_text("Open a file, drop one here, or print to “PrintStudio” from any app.");
            self.print_button.set_sensitive(false);
            return;
        };
        let out = match self.compute() {
            Ok(o) => o,
            Err(e) => {
                self.preview.clear();
                self.output.replace(None);
                self.summary.set_text("");
                self.set_status(&e, true);
                self.print_button.set_sensitive(false);
                return;
            }
        };
        self.set_status("", false);
        self.print_button.set_sensitive(!self.printers.borrow().is_empty() && !self.busy.get());
        // n-up layouts always fit the pages into their cells.
        self.orientation_row.set_sensitive(out.job.nup == 1);
        self.scaling_row.set_sensitive(out.job.nup == 1);

        let sheets = Rc::new(out.sheets.clone());
        let (layout, fallback) = (out.layout, out.fallback);
        let (src1, sheets1) = (source.clone(), sheets.clone());
        let (src2, sheets2) = (source, sheets);
        self.preview.set_items(
            &out.labels,
            Box::new(move |pos, px| {
                let (side, rotation) = &sheets1[pos as usize];
                render_sheet(&src1, &SheetSpec { side, rotation: *rotation, layout, fallback }, px)
            }),
            Box::new(move |pos| {
                let (side, rotation) = &sheets2[pos as usize];
                displayed_sheet_size(&src2, &SheetSpec { side, rotation: *rotation, layout, fallback })
            }),
        );
        let sheet_count = out.passes[0].sides.len();
        let plural = if sheet_count == 1 { "" } else { "s" };
        self.summary.set_text(&if out.job.duplex {
            format!("{sheet_count} sheet{plural}, printed in two passes (fronts, then backs)")
        } else {
            format!("{sheet_count} sheet{plural} of paper, shown in print order")
        });
        self.output.replace(Some(out));
    }

    // Thumbnails: zoom, page selection, full-size viewer

    fn zoom_step(&self, direction: i32) {
        let size = self.zoom.value();
        self.zoom.set_value(if direction > 0 { size * ZOOM_STEP } else { size / ZOOM_STEP });
    }

    fn on_page_selection(&self) {
        let rows = self.pages.selected();
        let n = self.pages.model.n_items() as usize;
        self.update_selection_label(rows.len(), n);
        if self.syncing.get() {
            return;
        }
        let text = if rows.is_empty() || rows.len() == n { String::new() } else { pipeline::format_ranges(&rows) };
        self.syncing.set(true);
        self.range_row.set_text(&text);
        self.syncing.set(false);
    }

    fn select_from_ranges(&self, text: &str) {
        let n = self.pages.model.n_items() as usize;
        let wanted = if text.trim().is_empty() {
            Vec::new()
        } else {
            match pipeline::parse_ranges(text, n) {
                Ok(p) => p,
                Err(_) => return, // half-typed range; keep the current selection
            }
        };
        self.syncing.set(true);
        self.pages.set_selected(&wanted);
        self.syncing.set(false);
        let mut unique = wanted.clone();
        unique.sort_unstable();
        unique.dedup();
        self.update_selection_label(unique.len(), n);
    }

    fn update_selection_label(&self, selected: usize, total: usize) {
        self.selection_label.set_text(&match (selected, total) {
            (_, 0) => String::new(),
            (s, t) if s == 0 || s == t => format!("All {t} pages will print"),
            (s, t) => format!("{s} of {t} pages selected"),
        });
    }

    fn view_preview(&self, pos: u32) {
        let (Some(out), Some(doc)) = (self.output.borrow().clone(), self.doc.borrow().as_ref().map(|d| (d.source.clone(), d.title.clone()))) else { return };
        let (source, title) = doc;
        let sheets = Rc::new(out.sheets);
        let (layout, fallback) = (out.layout, out.fallback);
        let (s1, sh1, s2, sh2) = (source.clone(), sheets.clone(), source, sheets.clone());
        viewer::open(
            &self.window,
            &format!("Print preview — {title}"),
            sheets.len() as u32,
            pos,
            Rc::new(move |i, k| {
                let (side, rotation) = &sh1[i as usize];
                render_scaled(&s1, &SheetSpec { side, rotation: *rotation, layout, fallback }, k)
            }),
            Rc::new(move |i| {
                let (side, rotation) = &sh2[i as usize];
                displayed_sheet_size(&s2, &SheetSpec { side, rotation: *rotation, layout, fallback })
            }),
            out.labels,
        );
    }

    fn view_source(&self, pos: u32) {
        let Some((source, title)) = self.doc.borrow().as_ref().map(|d| (d.source.clone(), d.title.clone())) else { return };
        let (s1, s2) = (source.clone(), source.clone());
        viewer::open(
            &self.window,
            &title,
            source.n_pages() as u32,
            pos,
            Rc::new(move |i, k| {
                let side = vec![i as usize];
                render_scaled(&s1, &SheetSpec::page(&side), k)
            }),
            Rc::new(move |i| s2.sizes[i as usize]),
            Vec::new(),
        );
    }

    // Output

    /// Printer options for CUPS. Scaling and orientation are already done:
    /// the pages we send are laid out on the chosen paper.
    fn cups_options(&self) -> Vec<(String, String)> {
        self.read_ppd().into_iter().collect()
    }

    fn print(self: &Rc<Self>) {
        if self.busy.get() || !self.print_button.is_sensitive() {
            return;
        }
        let (Some(printer), Some(out), Some(src_path)) =
            (self.printer_name(), self.compute().ok(), self.doc.borrow().as_ref().map(|d| d.path.clone()))
        else {
            return;
        };
        let title = self.doc.borrow().as_ref().map(|d| d.title.clone()).unwrap_or_default();
        let options = self.cups_options();
        self.remember_printer_settings();

        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            w.set_busy(true, "Preparing pages…");
            let (tmp, passes, job, layout) = (w.tmp.clone(), out.passes.clone(), out.job.clone(), out.layout);
            // Laid out on known paper: send it print-ready, past CUPS's pdftopdf.
            let ready = !layout.passthrough();
            let files = gio::spawn_blocking(move || {
                let doc = pdfout::load(&src_path)?;
                pdfout::write_passes(&doc, &passes, &job, &layout, &tmp, "job")
            })
            .await
            .unwrap_or_else(|_| Err("Preparing the pages failed".into()));
            let files = match files {
                Ok(f) => f,
                Err(e) => {
                    w.set_busy(false, "");
                    return w.error("Print failed", &e);
                }
            };
            if !out.job.duplex {
                w.set_busy(true, "Sending to printer…");
                let result = submit(&printer, &files[0], &title, &options, ready).await;
                w.set_busy(false, "");
                match result {
                    Ok(_) => w.window.close(),
                    Err(e) => w.error("Print failed", &e),
                }
                return;
            }
            w.set_busy(false, "");
            let sheets = out.passes[0].sides.len();
            if w.duplex_flow(&printer, &files, &title, &options, sheets, ready).await {
                w.window.close();
            }
        }));
    }

    fn print_calibration(self: &Rc<Self>) {
        let Some(printer) = self.printer_name() else { return };
        let current = self.read_job();
        let job = JobOptions {
            reverse: current.reverse,
            duplex: true,
            backs_reverse: current.backs_reverse,
            backs_rotate: current.backs_rotate,
            ..JobOptions::default()
        };
        let paper = self.paper().unwrap_or(testpdf::A4);
        let options = self.read_ppd().into_iter().collect::<Vec<_>>();
        self.remember_printer_settings();
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            let tmp = w.tmp.clone();
            let files = gio::spawn_blocking(move || {
                let doc = testpdf::duplex_test(paper);
                let passes = pipeline::plan(4, &job)?;
                // The test sheets are already made at paper size.
                let layout = SheetLayout::new(&job, None, 0.0);
                pdfout::write_passes(&doc, &passes, &job, &layout, &tmp, "calibration")
            })
            .await
            .unwrap_or_else(|_| Err("Preparing the test failed".into()));
            match files {
                Ok(files) => {
                    if w.duplex_flow(&printer, &files, "Two-sided calibration", &options, 2, false).await {
                        w.set_status("Calibration printed. Check both sheets and adjust the two-sided options if needed.", false);
                    }
                }
                Err(e) => w.error("Print failed", &e),
            }
        }));
    }

    /// Print fronts, guide the user through reloading, print backs. True if both passes were sent.
    /// `ready`: the files are already laid out on the paper (see `cups::FORMAT_PRINT_READY_PDF`).
    async fn duplex_flow(self: &Rc<Self>, printer: &str, files: &[PathBuf], title: &str,
                         options: &[(String, String)], sheets: usize, ready: bool) -> bool {
        self.set_busy(true, "Sending front sides…");
        let front = submit(printer, &files[0], &format!("{title} (fronts)"), options, ready).await;
        self.set_busy(false, "");
        let job_id = match front {
            Ok(id) => id,
            Err(e) => {
                self.error("Print failed", &format!("Couldn't print the front sides:\n{e}"));
                return false;
            }
        };

        let note = self.note_row.text().trim().to_string();
        let mut body = INSTRUCTIONS.to_string();
        if !note.is_empty() {
            body += &format!("\n\n<b>Your note:</b> {}", glib::markup_escape_text(&note));
        }
        let plural = if sheets == 1 { "" } else { "s" };
        let dialog = adw::AlertDialog::new(Some(&format!("Printing front sides ({sheets} sheet{plural})")), Some(&body));
        dialog.set_body_use_markup(true);
        dialog.add_responses(&[("cancel", "Cancel"), ("back", "Print back sides")]);
        dialog.set_response_appearance("back", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("back"));
        dialog.set_close_response("cancel");
        let status = gtk::Label::new(Some(&format!("Job {job_id}: queued…")));
        status.add_css_class("dim-label");
        status.set_wrap(true);
        dialog.set_extra_child(Some(&status));
        let weak_status = status.downgrade();
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(status) = weak_status.upgrade() else { return glib::ControlFlow::Break };
            let state = cups::job_state(job_id);
            status.set_text(&match state {
                cups::JobState::Done => format!("✔ Job {job_id}: front sides printed. Reload the stack, then print the back sides."),
                cups::JobState::Printing => format!("Job {job_id}: printing front sides…"),
                s => format!("Job {job_id}: {}", s.describe()),
            });
            if state.is_final() { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
        });

        if dialog.choose_future(Some(&self.window)).await != "back" {
            return false;
        }
        self.set_busy(true, "Sending back sides…");
        let back = submit(printer, &files[1], &format!("{title} (backs)"), options, ready).await;
        self.set_busy(false, "");
        match back {
            Ok(_) => true,
            Err(e) => {
                self.error("Print failed", &format!("Couldn't print the back sides:\n{e}"));
                false
            }
        }
    }

    fn save_pdf(self: &Rc<Self>) {
        let (Some(out), Some((src_path, title))) = (self.compute().ok(), self.doc.borrow().as_ref().map(|d| (d.path.clone(), d.title.clone()))) else {
            return;
        };
        let stem = Path::new(&title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
        let dialog = gtk::FileDialog::builder().title("Save output as PDF").initial_name(format!("{stem}-print.pdf")).build();
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            let Ok(file) = dialog.save_future(Some(&w.window)).await else { return };
            let Some(dest) = file.path() else { return };
            w.set_busy(true, "Saving…");
            let shown = dest.display().to_string();
            let result = gio::spawn_blocking(move || {
                let doc = pdfout::load(&src_path)?;
                pdfout::write_combined(&doc, &out.passes, &out.job, &out.layout, &dest)
            })
            .await
            .unwrap_or_else(|_| Err("Saving failed".into()));
            w.set_busy(false, "");
            match result {
                Ok(()) => w.set_status(&format!("Saved {shown}"), false),
                Err(e) => w.error("Couldn't save the PDF", &e),
            }
        }));
    }

    // Feedback

    fn set_status(&self, text: &str, error: bool) {
        self.status.set_text(text);
        self.status.set_visible(!text.is_empty());
        if error {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
    }

    fn set_busy(&self, busy: bool, message: &str) {
        self.busy.set(busy);
        self.spinner.set_spinning(busy);
        self.print_button.set_sensitive(!busy && self.output.borrow().is_some());
        // Show progress while busy; clear it afterwards, but leave other messages alone.
        if busy || self.status.text().ends_with('…') {
            self.set_status(message, false);
        }
    }

    fn error(&self, heading: &str, message: &str) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(message));
        dialog.add_response("ok", "OK");
        dialog.present(Some(&self.window));
    }
}

/// Send a file to CUPS without blocking the window (big files take a moment to upload).
async fn submit(printer: &str, path: &Path, title: &str, options: &[(String, String)], ready: bool) -> Result<i32, String> {
    let (printer, path, title, options) = (printer.to_string(), path.to_path_buf(), title.to_string(), options.to_vec());
    let format = ready.then_some(cups::FORMAT_PRINT_READY_PDF);
    gio::spawn_blocking(move || cups::print_file(&printer, &path, &title, &options, format))
        .await
        .unwrap_or_else(|_| Err("Sending the job failed".into()))
}

/// Development only: `PRINTSTUDIO_DEVSCRIPT="dark;tab=pages;range=1-3;shot=/tmp/a.png;quit"`
/// drives the window step by step and saves screenshots. Built with `--features devshot`.
#[cfg(feature = "devshot")]
impl Win {
    pub fn run_dev_script(self: &Rc<Self>, script: String) {
        let steps: Vec<String> = script.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        self.dev_step(Rc::new(steps), 0);
    }

    fn dev_step(self: &Rc<Self>, steps: Rc<Vec<String>>, i: usize) {
        let Some(step) = steps.get(i) else { return };
        let (cmd, arg) = step.split_once('=').unwrap_or((step.as_str(), ""));
        let mut delay = 700;
        match cmd {
            "wait" => delay = arg.parse().unwrap_or(500),
            "dark" | "light" | "system" => apply_color_scheme(cmd),
            // Go through the ☰ menu's action, like a click on the menu item.
            "scheme" => {
                let _ = WidgetExt::activate_action(&self.window, "win.color-scheme", Some(&arg.to_variant()));
                let sm = adw::StyleManager::default();
                println!("scheme {arg}: color_scheme={:?} dark={} system_supports={}", sm.color_scheme(), sm.is_dark(), sm.system_supports_color_schemes());
            }
            "tab" => self.stack.set_visible_child_name(arg),
            "range" => self.range_row.set_text(arg),
            "nup" => self.nup_row.set_selected(NUP_CHOICES.iter().position(|n| n.to_string() == arg).unwrap_or(0) as u32),
            "duplex" => self.duplex_row.set_enable_expansion(arg == "1"),
            "reverse" => self.reverse_row.set_active(arg == "1"),
            "orientation" => self.orientation_row.set_selected(ORIENTATIONS.iter().position(|o| o.0.to_lowercase() == arg).unwrap_or(0) as u32),
            "scaling" => self.scaling_row.set_selected(SCALINGS.iter().position(|s| s.0.to_lowercase().starts_with(arg)).unwrap_or(0) as u32),
            "paper" => {
                let rows = self.ppd_rows.borrow();
                if let Some((_, row, values)) = rows.iter().find(|(k, _, _)| k == "PageSize") {
                    row.set_selected(values.iter().position(|v| v == arg).unwrap_or(0) as u32);
                }
            }
            "size" => self.zoom.set_value(arg.parse().unwrap_or(210.0)),
            "view" => self.view_preview(arg.parse().unwrap_or(0)),
            "viewsrc" => self.view_source(arg.parse().unwrap_or(0)),
            "print-state" => println!("scale={} range={:?} selected={:?} summary={:?} status={:?} labels={:?}",
                self.window.scale_factor(), self.range_row.text(), self.pages.selected(), self.summary.text(), self.status.text(),
                self.output.borrow().as_ref().map(|o| o.labels.clone())),
            "shot" => dev_screenshot(self.window.upcast_ref(), arg, 1.0),
            // Capture at 125%, like a fractionally scaled screen.
            "shot125" => dev_screenshot(self.window.upcast_ref(), arg, 1.25),
            "shot-top" => {
                // The most recently opened toplevel (e.g. the page viewer).
                let tops = gtk::Window::list_toplevels();
                if let Some(top) = tops.iter().filter_map(|w| w.clone().downcast::<gtk::Window>().ok()).find(|w| w != self.window.upcast_ref::<gtk::Window>() && w.is_visible()) {
                    dev_screenshot(top.upcast_ref(), arg, 1.0);
                }
            }
            "key-top" => {
                let tops = gtk::Window::list_toplevels();
                if let Some(top) = tops.iter().filter_map(|w| w.clone().downcast::<gtk::Window>().ok()).find(|w| w != self.window.upcast_ref::<gtk::Window>() && w.is_visible()) {
                    if arg == "close" { top.close(); }
                }
            }
            "count-tops" => println!("visible toplevels: {}", gtk::Window::list_toplevels().iter().filter(|w| w.is_visible()).count()),
            "open" => self.open_path(PathBuf::from(arg)),
            "tick" => {
                let ok = self.pages.click_check(arg.parse().unwrap_or(0));
                println!("tick {arg}: {}", if ok { "clicked" } else { "card not on screen" });
            }
            "prefs-scroll" => {
                let mut child = self.prefs.first_child();
                while let Some(w) = child {
                    if let Ok(sw) = w.clone().downcast::<gtk::ScrolledWindow>() {
                        sw.vadjustment().set_value(arg.parse().unwrap_or(0.0));
                        break;
                    }
                    child = w.first_child();
                }
            }
            "expand-duplex" => self.duplex_row.set_expanded(true),
            "stamp" => {
                let up = std::fs::read_to_string("/proc/self/stat").ok().and_then(|s| s.split_whitespace().nth(21)?.parse::<f64>().ok());
                let boot = std::fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
                if let (Some(start), Some(now)) = (up, boot) {
                    println!("stamp {arg}: {:.0} ms since process start; thumbnails rendered: preview {}, pages {}",
                        (now - start / 100.0) * 1000.0, self.preview.renders.get(), self.pages.renders.get());
                }
            }
            "timed-rebuild" => {
                let t = std::time::Instant::now();
                self.rebuild();
                println!("rebuild took {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
            }
            "quit" => self.window.close(),
            other => eprintln!("devscript: unknown step {other:?}"),
        }
        glib::timeout_add_local_once(Duration::from_millis(delay), clone!(#[weak(rename_to = w)] self, move || w.dev_step(steps, i + 1)));
    }
}

#[cfg(feature = "devshot")]
fn dev_screenshot(widget: &gtk::Widget, path: &str, scale: f32) {
    let (w, h) = (widget.width(), widget.height());
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(scale, scale);
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let (w, h) = ((w as f32 * scale) as i32, (h as f32 * scale) as i32);
    let (Some(node), Some(renderer)) = (snapshot.to_node(), widget.native().and_then(|n| n.renderer())) else {
        eprintln!("devscript: nothing to capture");
        return;
    };
    let texture = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
    match texture.save_to_png(path) {
        Ok(()) => println!("saved {path} ({w}x{h})"),
        Err(e) => eprintln!("devscript: {e}"),
    }
}
