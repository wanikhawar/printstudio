//! The Print Studio window: the print preview in the middle, settings in a
//! sidebar on the right.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib::clone;
use gtk::{gdk, gio, glib};

use super::thumbs::{MAX_SIZE, MIN_SIZE, Thumbs};
use super::{APP_ID, apply_color_scheme, art, fit_to_monitor, viewer};
use printstudio::config::{Config, PpdValues, merge_job};
use printstudio::cups::{self, PrinterInfo};
use printstudio::geometry::SheetLayout;
use printstudio::pipeline::{self, BLANK, JobOptions, NUP_CHOICES, Orientation, PageSet, Pass, Scaling, Side};
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
/// Sheets folded into one booklet (0: all of them).
const BOOKLET_SHEETS: [(&str, u32); 8] =
    [("All in one", 0), ("1", 1), ("2", 2), ("3", 3), ("4", 4), ("5", 5), ("6", 6), ("8", 8)];
const BINDINGS: [(&str, bool); 2] = [("Left (left-to-right text)", false), ("Right (right-to-left text)", true)];
const INSTRUCTIONS: [&str; 4] = [
    "Wait until <b>all front sides</b> have finished printing.",
    "Take the whole stack out of the output tray. <b>Don't reorder it.</b>",
    "Put it back in the paper tray so the <b>blank sides</b> get printed next.",
    "Click <b>Print Back Sides</b>.",
];
const FIRST_TIME: &str = "First time with this printer? Use <i>Print Test</i> under <i>Manual two-sided printing</i> to check how the stack goes back in.";

/// A document in the job, as the user added it.
struct Entry {
    /// The PDF (converted, if the user opened something else).
    pdf: PathBuf,
    title: String,
    pages: usize,
}

/// What gets printed: one document, or several joined into one PDF.
struct Doc {
    path: PathBuf,
    title: String,
    source: Rc<Source>,
    /// Page count of each document, in order.
    doc_pages: Vec<usize>,
}

/// Something sent to Print Studio while a window was already open.
#[derive(Clone)]
pub enum Incoming {
    File(PathBuf),
    Spool(SpoolJob),
}

impl Incoming {
    fn title(&self) -> String {
        match self {
            Incoming::File(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            Incoming::Spool(job) if job.title.is_empty() => "Untitled".into(),
            Incoming::Spool(job) => job.title.clone(),
        }
    }
}

/// What the preview currently shows (and what Print will print).
#[derive(Clone)]
struct Output {
    job: JobOptions,
    passes: Vec<Pass>,
    sheets: Vec<(Side, u32)>,
    /// The sheets as the preview shows them: like `sheets`, except that
    /// booklet sheets are turned to read like the open booklet.
    view: Vec<(Side, u32)>,
    labels: Vec<String>,
    layout: SheetLayout,
    fallback: (f64, f64),
}

impl Output {
    /// Sheets of paper used.
    fn paper(&self) -> usize {
        self.passes[0].sides.len()
    }

    /// Pages printed, counting copies (one per sheet if printed plainly).
    fn page_count(&self) -> usize {
        self.sheets.iter().map(|(s, _)| s.iter().filter(|&&p| p != BLANK).count()).sum()
    }
}

pub struct Win {
    app: adw::Application,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    split: adw::OverlaySplitView,
    switcher: adw::ViewSwitcher,
    body: gtk::Stack,
    stack: adw::ViewStack,
    preview: Rc<Thumbs>,
    pages: Rc<Thumbs>,
    selection_label: gtk::Label,
    info: gtk::Label,
    zoom: gtk::Scale,

    printer_row: adw::ComboRow,
    preset_row: adw::ComboRow,
    docs_group: adw::PreferencesGroup,
    doc_rows: RefCell<Vec<adw::ActionRow>>,
    separate_row: adw::SwitchRow,
    range_row: adw::EntryRow,
    page_set_row: adw::ComboRow,
    copies_row: adw::SpinRow,
    collate_row: adw::SwitchRow,
    reverse_row: adw::SwitchRow,
    nup_row: adw::ComboRow,
    rotate_row: adw::ComboRow,
    orientation_row: adw::ComboRow,
    scaling_row: adw::ComboRow,
    booklet_row: adw::ExpanderRow,
    booklet_sheets_row: adw::ComboRow,
    binding_row: adw::ComboRow,
    gutter_row: adw::SpinRow,
    duplex_row: adw::ExpanderRow,
    backs_reverse_row: adw::SwitchRow,
    backs_rotate_row: adw::SwitchRow,
    note_row: adw::EntryRow,
    prefs: adw::PreferencesPage,
    ppd_groups: RefCell<Vec<adw::PreferencesGroup>>,
    ppd_rows: RefCell<Vec<(String, adw::ComboRow, Vec<String>)>>,

    sheets_label: gtk::Label,
    detail_label: gtk::Label,
    spinner: gtk::Spinner,
    print_button: gtk::Button,

    cfg: RefCell<Config>,
    printers: RefCell<Vec<String>>,
    presets: RefCell<Vec<String>>,
    printer_info: RefCell<PrinterInfo>,
    entries: RefCell<Vec<Entry>>,
    doc: RefCell<Option<Doc>>,
    /// Bumped whenever the documents change, so a slow merge that's been overtaken is dropped.
    doc_generation: Cell<u32>,
    output: RefCell<Option<Output>>,
    /// Captured print jobs shown in this window; their spool files go when it closes.
    spool_jobs: RefCell<Vec<SpoolJob>>,
    incoming: RefCell<VecDeque<Incoming>>,
    asking: Cell<bool>,
    /// Two-sided setting from before booklet mode switched it on.
    duplex_before_booklet: Cell<Option<bool>>,
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

fn group(title: &str) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    g.set_title(title);
    g
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    b
}

fn set_combo_items(row: &adw::ComboRow, items: &[String]) {
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk::StringList::new(&items)));
}

fn temp_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("printstudio-{}-{}", std::process::id(), glib::monotonic_time()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The window that should receive new jobs: the one used most recently.
fn latest_window() -> Option<Rc<Win>> {
    OPEN.with(|open| {
        let open = open.borrow();
        open.iter().find(|w| w.window.is_active()).or_else(|| open.last()).cloned()
    })
}

/// Open a file or captured job: in an empty window if there is one,
/// otherwise ask the window in use whether to add it or open it separately.
pub fn deliver(app: &adw::Application, item: Option<Incoming>) {
    match (latest_window(), item) {
        (Some(w), Some(item)) if !w.entries.borrow().is_empty() || w.busy.get() => w.ask_incoming(item),
        (Some(w), Some(item)) => {
            w.take(item);
            w.present();
        }
        (Some(w), None) => w.present(),
        (None, item) => Win::new(app, item).present(),
    }
}

impl Win {
    pub fn new(app: &adw::Application, item: Option<Incoming>) -> Rc<Win> {
        let cfg = Config::load();
        apply_color_scheme(cfg.ui("color_scheme").and_then(|v| v.as_str()).unwrap_or("system"));

        let window = adw::ApplicationWindow::new(app);
        window.set_title(Some("Print Studio"));
        window.set_icon_name(Some(APP_ID));
        window.set_size_request(360, 480);
        fit_to_monitor(window.upcast_ref(), None, 1240, 820, 0.88);

        // ── Content: the preview ──────────────────────────────────────────
        let preview = Thumbs::new(false);
        preview.view.set_tooltip_text(Some("Exactly what will print, in print order. Double-click to zoom in."));
        preview.scrolled.add_css_class("desk");
        let pages = Thumbs::new(true);
        pages.view.set_tooltip_text(Some("Tick the pages to print (or Ctrl+click, Shift+click, drag). Double-click to zoom in."));
        pages.scrolled.add_css_class("desk");

        let selection_label = gtk::Label::new(None);
        selection_label.set_hexpand(true);
        selection_label.set_xalign(0.0);
        selection_label.add_css_class("dim-label");
        let sel_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        sel_bar.add_css_class("toolbar");
        sel_bar.set_margin_start(6);
        sel_bar.append(&selection_label);
        let select_all = gtk::Button::with_label("Select All");
        let clear = gtk::Button::with_label("Clear");
        clear.set_tooltip_text(Some("No selection means every page prints"));
        let invert = gtk::Button::with_label("Invert");
        for b in [&select_all, &clear, &invert] {
            b.add_css_class("flat");
            sel_bar.append(b);
        }
        let pages_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pages_box.append(&sel_bar);
        pages_box.append(&pages.scrolled);

        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(&preview.scrolled, Some("preview"), "Preview", "printer-symbolic");
        stack.add_titled_with_icon(&pages_box, Some("pages"), "Pages", "view-grid-symbolic");
        stack.set_vexpand(true);
        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();

        let info = gtk::Label::new(None);
        info.set_hexpand(true);
        info.set_xalign(0.0);
        info.set_ellipsize(gtk::pango::EllipsizeMode::End);
        info.add_css_class("dim-label");
        let zoom_out = icon_button("zoom-out-symbolic", "Smaller thumbnails (Ctrl+−, or Ctrl+scroll)");
        zoom_out.set_action_name(Some("win.zoom-out"));
        let zoom = gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_SIZE as f64, MAX_SIZE as f64, 10.0);
        zoom.set_draw_value(false);
        zoom.set_tooltip_text(Some("Thumbnail size"));
        let zoom_in = icon_button("zoom-in-symbolic", "Bigger thumbnails (Ctrl++, or Ctrl+scroll)");
        zoom_in.set_action_name(Some("win.zoom-in"));
        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        bottom.add_css_class("toolbar");
        bottom.add_css_class("zoom-bar");
        bottom.set_margin_start(6);
        bottom.append(&info);
        bottom.append(&zoom_out);
        bottom.append(&zoom);
        bottom.append(&zoom_in);

        let empty = adw::StatusPage::new();
        let has_icon = gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(APP_ID));
        empty.set_icon_name(Some(if has_icon { APP_ID } else { "printer-symbolic" }));
        empty.set_title("Nothing to Print Yet");
        empty.set_description(Some("Print to <b>PrintStudio</b> from any app, or open a document here.\nYou can also drop files anywhere in this window."));
        let open_button = gtk::Button::with_label("Open a Document…");
        open_button.add_css_class("pill");
        open_button.add_css_class("suggested-action");
        open_button.set_halign(gtk::Align::Center);
        open_button.set_action_name(Some("win.open"));
        empty.set_child(Some(&open_button));
        empty.add_css_class("desk");

        let loaded = gtk::Box::new(gtk::Orientation::Vertical, 0);
        loaded.append(&stack);
        loaded.append(&bottom);
        let body = gtk::Stack::new();
        body.set_transition_type(gtk::StackTransitionType::Crossfade);
        body.add_named(&empty, Some("empty"));
        body.add_named(&loaded, Some("loaded"));

        let add_button = icon_button("list-add-symbolic", "Add a document (Ctrl+O)");
        add_button.set_action_name(Some("win.open"));
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Show settings (F9)")
            .build();
        let content_header = adw::HeaderBar::new();
        content_header.pack_start(&add_button);
        content_header.set_title_widget(Some(&switcher));
        content_header.pack_end(&sidebar_button);

        let banner = adw::Banner::new("");
        let content = adw::ToolbarView::new();
        content.add_top_bar(&content_header);
        content.add_top_bar(&banner);
        content.set_content(Some(&body));

        // ── Sidebar: settings ─────────────────────────────────────────────
        let prefs = adw::PreferencesPage::new();

        let printer_group = adw::PreferencesGroup::new();
        let printer_row = combo("Printer", &[]);
        printer_group.add(&printer_row);
        let preset_row = combo("Preset", &[]);
        let save_preset = icon_button("document-save-symbolic", "Save the current settings as a preset");
        let delete_preset = icon_button("user-trash-symbolic", "Delete this preset");
        preset_row.add_suffix(&save_preset);
        preset_row.add_suffix(&delete_preset);
        printer_group.add(&preset_row);
        prefs.add(&printer_group);

        let docs_group = group("Documents");
        docs_group.set_description(Some("Printed in this order. Drag to reorder."));
        let add_doc = icon_button("list-add-symbolic", "Add a document to this job");
        add_doc.set_action_name(Some("win.open"));
        docs_group.set_header_suffix(Some(&add_doc));
        let separate_row = switch("Start each on a new sheet", "So documents never share a sheet of paper");
        docs_group.add(&separate_row);
        docs_group.set_visible(false);
        prefs.add(&docs_group);

        let pages_group = group("Pages");
        let range_row = adw::EntryRow::new();
        range_row.set_title("Pages (e.g. 1-3, 7, 10-)");
        let page_set_row = combo("Print", &PAGE_SETS.map(|(l, _)| l));
        let copies_row = adw::SpinRow::with_range(1.0, 999.0, 1.0);
        copies_row.set_title("Copies");
        let collate_row = switch("Collate", "1, 2, 3, 1, 2, 3 instead of 1, 1, 2, 2, 3, 3");
        let reverse_row = switch("Reverse order", "Last page first, for printers that stack pages face up");
        for row in [range_row.upcast_ref::<gtk::Widget>(), page_set_row.upcast_ref(), copies_row.upcast_ref(),
                    collate_row.upcast_ref(), reverse_row.upcast_ref()] {
            pages_group.add(row);
        }
        prefs.add(&pages_group);

        let layout_group = group("Layout");
        let nup_labels: Vec<String> = NUP_CHOICES.iter().map(|n| n.to_string()).collect();
        let nup_row = combo("Pages per sheet", &nup_labels.iter().map(String::as_str).collect::<Vec<_>>());
        let orientation_row = combo("Orientation", &ORIENTATIONS.map(|(l, _)| l));
        orientation_row.set_subtitle("Automatic turns the paper to match each page");
        let scaling_row = combo("Scaling", &SCALINGS.map(|(l, _)| l));
        let rotate_row = combo("Rotate", &ROTATIONS.map(|(l, _)| l));
        for row in [nup_row.upcast_ref::<gtk::Widget>(), orientation_row.upcast_ref(), scaling_row.upcast_ref(), rotate_row.upcast_ref()] {
            layout_group.add(row);
        }
        let booklet_row = adw::ExpanderRow::new();
        booklet_row.set_title("Booklet");
        booklet_row.set_subtitle("Two pages per side, ordered so the folded stack reads like a book");
        booklet_row.set_show_enable_switch(true);
        booklet_row.set_enable_expansion(false);
        let booklet_sheets_row = combo("Sheets per booklet", &BOOKLET_SHEETS.map(|(l, _)| l));
        booklet_sheets_row.set_subtitle("Thick booklets fold better as several thin ones");
        let binding_row = combo("Binding", &BINDINGS.map(|(l, _)| l));
        let gutter_row = adw::SpinRow::with_range(0.0, 30.0, 1.0);
        gutter_row.set_title("Gutter (mm)");
        gutter_row.set_subtitle("Extra space at the fold");
        booklet_row.add_row(&booklet_sheets_row);
        booklet_row.add_row(&binding_row);
        booklet_row.add_row(&gutter_row);
        layout_group.add(&booklet_row);
        prefs.add(&layout_group);

        let duplex_group = group("Two-Sided");
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
        let calibration_button = gtk::Button::with_label("Print Test");
        calibration_button.set_valign(gtk::Align::Center);
        calibration_row.add_suffix(&calibration_button);
        duplex_row.add_row(&backs_reverse_row);
        duplex_row.add_row(&backs_rotate_row);
        duplex_row.add_row(&note_row);
        duplex_row.add_row(&calibration_row);
        duplex_group.add(&duplex_row);
        prefs.add(&duplex_group);

        // Footer: how much paper, and the buttons.
        let sheets_label = gtk::Label::new(None);
        sheets_label.add_css_class("title-3");
        sheets_label.set_xalign(0.0);
        let detail_label = gtk::Label::new(None);
        detail_label.add_css_class("caption");
        detail_label.add_css_class("dim-label");
        detail_label.set_xalign(0.0);
        detail_label.set_wrap(true);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        let summary_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        summary_text.set_hexpand(true);
        summary_text.append(&sheets_label);
        summary_text.append(&detail_label);
        let summary = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        summary.add_css_class("paper-summary");
        let paper_icon = gtk::Image::from_icon_name("printer-symbolic");
        paper_icon.add_css_class("paper-icon");
        paper_icon.set_valign(gtk::Align::Center);
        summary.append(&paper_icon);
        summary.append(&summary_text);
        summary.append(&spinner);

        let more_menu = gio::Menu::new();
        more_menu.append(Some("Save as PDF…"), Some("win.save-pdf"));
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&more_menu)
            .tooltip_text("More")
            .valign(gtk::Align::Center)
            .build();
        more.add_css_class("circular");
        let cancel = gtk::Button::with_label("Cancel");
        cancel.set_action_name(Some("window.close"));
        cancel.add_css_class("pill");
        let print_button = gtk::Button::with_label("Print");
        print_button.add_css_class("suggested-action");
        print_button.add_css_class("pill");
        print_button.set_action_name(Some("win.print"));
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        buttons.append(&more);
        buttons.append(&spacer);
        buttons.append(&cancel);
        buttons.append(&print_button);
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 12);
        footer.add_css_class("sidebar-footer");
        footer.append(&summary);
        footer.append(&buttons);

        let menu = gio::Menu::new();
        let appearance = gio::Menu::new();
        appearance.append(Some("Follow System"), Some("win.color-scheme::system"));
        appearance.append(Some("Light"), Some("win.color-scheme::light"));
        appearance.append(Some("Dark"), Some("win.color-scheme::dark"));
        menu.append_section(Some("Appearance"), &appearance);
        let about = gio::Menu::new();
        about.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        about.append(Some("About Print Studio"), Some("win.about"));
        menu.append_section(None, &about);
        let menu_button = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Main Menu").build();
        menu_button.set_primary(true);
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Print Settings", "")));
        sidebar_header.pack_end(&menu_button);

        let sidebar = adw::ToolbarView::new();
        sidebar.add_top_bar(&sidebar_header);
        sidebar.set_content(Some(&prefs));
        sidebar.add_bottom_bar(&footer);
        sidebar.set_bottom_bar_style(adw::ToolbarStyle::Raised);

        let split = adw::OverlaySplitView::new();
        split.set_sidebar_position(gtk::PackType::End);
        split.set_min_sidebar_width(360.0);
        split.set_max_sidebar_width(430.0);
        split.set_sidebar_width_fraction(0.36);
        split.set_content(Some(&content));
        split.set_sidebar(Some(&sidebar));
        split.bind_property("show-sidebar", &sidebar_button, "active").bidirectional().sync_create().build();
        // Window buttons sit in whichever header bar is at that edge.
        let sync_buttons = clone!(#[weak] content_header, #[weak] sidebar_header, move |split: &adw::OverlaySplitView| {
            let sidebar_visible = split.shows_sidebar() && !split.is_collapsed();
            content_header.set_show_end_title_buttons(!sidebar_visible);
            sidebar_header.set_show_start_title_buttons(split.is_collapsed());
            sidebar_header.set_show_end_title_buttons(sidebar_visible || split.is_collapsed());
        });
        sync_buttons(&split);
        split.connect_show_sidebar_notify(sync_buttons.clone());
        split.connect_collapsed_notify(sync_buttons);

        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 820sp").unwrap());
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        let win = Rc::new(Win {
            app: app.clone(),
            window,
            toasts,
            banner,
            split,
            switcher,
            body,
            stack,
            preview,
            pages,
            selection_label,
            info,
            zoom,
            printer_row,
            preset_row,
            docs_group,
            doc_rows: RefCell::new(Vec::new()),
            separate_row,
            range_row,
            page_set_row,
            copies_row,
            collate_row,
            reverse_row,
            nup_row,
            rotate_row,
            orientation_row,
            scaling_row,
            booklet_row,
            booklet_sheets_row,
            binding_row,
            gutter_row,
            duplex_row,
            backs_reverse_row,
            backs_rotate_row,
            note_row,
            prefs,
            ppd_groups: RefCell::new(Vec::new()),
            ppd_rows: RefCell::new(Vec::new()),
            sheets_label,
            detail_label,
            spinner,
            print_button,
            cfg: RefCell::new(cfg),
            printers: RefCell::new(Vec::new()),
            presets: RefCell::new(Vec::new()),
            printer_info: RefCell::new(PrinterInfo::default()),
            entries: RefCell::new(Vec::new()),
            doc: RefCell::new(None),
            doc_generation: Cell::new(0),
            output: RefCell::new(None),
            spool_jobs: RefCell::new(Vec::new()),
            incoming: RefCell::new(VecDeque::new()),
            asking: Cell::new(false),
            duplex_before_booklet: Cell::new(None),
            tmp: temp_dir(),
            applying: Cell::new(false),
            syncing: Cell::new(false),
            busy: Cell::new(false),
            rebuild_source: RefCell::new(None),
        });

        win.setup_actions();
        win.connect_signals(&select_all, &clear, &invert, &save_preset, &delete_preset, &calibration_button);
        win.restore_ui_prefs();
        win.load_printers();
        win.refresh_presets(None);

        if let Some(item) = item {
            win.take(item);
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
        add("save-pdf", |w| w.save_pdf());
        add("zoom-in", |w| w.zoom_step(1));
        add("zoom-out", |w| w.zoom_step(-1));
        add("toggle-sidebar", |w| w.split.set_show_sidebar(!w.split.shows_sidebar()));
        add("about", |w| w.show_about());
        add("shortcuts", |w| super::show_shortcuts(w.window.upcast_ref()));
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

    fn connect_signals(self: &Rc<Self>, select_all: &gtk::Button, clear: &gtk::Button, invert: &gtk::Button,
                       save_preset: &gtk::Button, delete_preset: &gtk::Button, calibration: &gtk::Button) {
        let w = self;
        for row in [&w.page_set_row, &w.nup_row, &w.rotate_row, &w.orientation_row, &w.scaling_row, &w.booklet_sheets_row, &w.binding_row] {
            row.connect_selected_notify(clone!(#[weak] w, move |_| w.schedule()));
        }
        for row in [&w.collate_row, &w.reverse_row, &w.backs_reverse_row, &w.backs_rotate_row, &w.separate_row] {
            row.connect_active_notify(clone!(#[weak] w, move |_| w.schedule()));
        }
        w.copies_row.connect_value_notify(clone!(#[weak] w, move |_| w.schedule()));
        w.gutter_row.connect_value_notify(clone!(#[weak] w, move |_| w.schedule()));
        w.duplex_row.connect_enable_expansion_notify(clone!(#[weak] w, move |row| {
            if !row.enables_expansion() && w.booklet_row.enables_expansion() && !w.applying.get() {
                // Booklets can't be printed on one side.
                w.duplex_before_booklet.set(None);
                w.booklet_row.set_enable_expansion(false);
                w.toast("Booklet turned off: booklets need two-sided printing");
            }
            w.schedule();
        }));
        w.booklet_row.connect_enable_expansion_notify(clone!(#[weak] w, move |row| {
            if !w.applying.get() {
                w.applying.set(true);
                if row.enables_expansion() {
                    w.duplex_before_booklet.set(Some(w.duplex_row.enables_expansion()));
                    w.duplex_row.set_enable_expansion(true);
                } else if let Some(before) = w.duplex_before_booklet.take() {
                    w.duplex_row.set_enable_expansion(before);
                }
                w.applying.set(false);
            }
            w.schedule();
        }));
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
        w.stack.connect_visible_child_name_notify(clone!(#[weak] w, move |_| w.update_info()));

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

        // Drop files anywhere to add them to the job.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_enter(clone!(#[weak] w, #[upgrade_or] gdk::DragAction::empty(), move |_, _, _| {
            w.body.add_css_class("drop-hover");
            gdk::DragAction::COPY
        }));
        drop.connect_leave(clone!(#[weak] w, move |_| w.body.remove_css_class("drop-hover")));
        drop.connect_drop(clone!(#[weak] w, #[upgrade_or] false, move |_, value, _, _| {
            w.body.remove_css_class("drop-hover");
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            for path in &paths {
                w.add_path(path.clone());
            }
            !paths.is_empty()
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
        for job in self.spool_jobs.borrow().iter() {
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
            booklet: self.booklet_row.enables_expansion(),
            booklet_sheets: BOOKLET_SHEETS.get(self.booklet_sheets_row.selected() as usize).map(|b| b.1).unwrap_or(0),
            booklet_rtl: BINDINGS.get(self.binding_row.selected() as usize).map(|b| b.1).unwrap_or(false),
            gutter_mm: self.gutter_row.value(),
            separate_docs: self.separate_row.is_active(),
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
        self.duplex_row.set_enable_expansion(job.duplex || job.booklet);
        self.backs_reverse_row.set_active(job.backs_reverse);
        self.backs_rotate_row.set_active(job.backs_rotate);
        self.booklet_row.set_enable_expansion(job.booklet);
        self.booklet_sheets_row.set_selected(BOOKLET_SHEETS.iter().position(|b| b.1 == job.booklet_sheets).unwrap_or(0) as u32);
        self.binding_row.set_selected(if job.booklet_rtl { 1 } else { 0 });
        self.gutter_row.set_value(job.gutter_mm);
        self.separate_row.set_active(job.separate_docs);
        if job.booklet && !job.duplex {
            self.duplex_before_booklet.set(Some(false));
        }
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
        self.printer_info.borrow().paper(values.get("PageSize").map(String::as_str))
    }

    /// Unprintable border for the chosen paper, from the printer driver.
    fn margin(&self) -> f64 {
        let values = self.read_ppd();
        self.printer_info.borrow().margin(values.get("PageSize").map(String::as_str))
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
            None => self.show_error("No printers found. Add one in your system's printer settings."),
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
        self.printer_info.replace(info);
        let (mut job, ppd) = self.cfg.borrow().printer_settings(&name);
        // These belong to the document, not the printer.
        job.page_ranges = current.page_ranges;
        job.copies = current.copies;
        job.booklet = current.booklet;
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

        let common = group("Printer Settings");
        for opt in info.options.iter().filter(|o| o.common) {
            common.add(&make_row(opt));
        }
        if info.options.is_empty() {
            common.set_description(Some("This printer doesn't list any options."));
        }
        groups.push(common);

        let advanced: Vec<_> = info.options.iter().filter(|o| !o.common).collect();
        if !advanced.is_empty() {
            let group = group("Advanced");
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
        self.toast(&format!("Preset “{name}” applied"));
        self.schedule();
    }

    fn save_preset(self: &Rc<Self>) {
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("e.g. Draft, grayscale, fast"));
        entry.set_text(&self.selected_preset().unwrap_or_default());
        entry.set_activates_default(true);
        let dialog = adw::AlertDialog::new(Some("Save Preset"), Some("Saves the current page and printer settings under a name."));
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
            w.toast(&format!("Preset “{name}” saved"));
        }));
    }

    fn delete_preset(self: &Rc<Self>) {
        let Some(name) = self.selected_preset() else { return };
        let dialog = adw::AlertDialog::new(Some("Delete Preset?"), Some(&format!("“{name}” will be removed.")));
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
        let (mut job, ppd, note) = (self.read_job(), self.read_ppd(), self.note_row.text().trim().to_string());
        // Two-sided was only switched on for the booklet; don't make it this printer's default.
        if let (true, Some(before)) = (job.booklet, self.duplex_before_booklet.get()) {
            job.duplex = before;
        }
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
        let title = if self.entries.borrow().is_empty() { "Open Document" } else { "Add Documents" };
        let dialog = gtk::FileDialog::builder().title(title).filters(&filters).default_filter(&filter).build();
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            if let Ok(files) = dialog.open_multiple_future(Some(&w.window)).await {
                for file in files.iter::<gio::File>().flatten() {
                    if let Some(path) = file.path() {
                        w.add_path(path);
                    }
                }
            }
        }));
    }

    /// Add a file or captured job to this window's job.
    pub fn take(self: &Rc<Self>, item: Incoming) {
        match item {
            Incoming::File(path) => self.add_path(path),
            Incoming::Spool(job) => {
                let title = Incoming::Spool(job.clone()).title();
                if self.entries.borrow().is_empty() {
                    self.copies_row.set_value(job.copies as f64);
                }
                self.spool_jobs.borrow_mut().push(job.clone());
                self.add_pdf(job.pdf.clone(), title);
            }
        }
    }

    fn add_path(self: &Rc<Self>, path: PathBuf) {
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if convert::is_pdf(&path) {
            self.add_pdf(path, title);
            return;
        }
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            w.set_busy(true, &format!("Converting {title} to PDF…"));
            let tmp = w.tmp.clone();
            let result = gio::spawn_blocking(move || convert::to_pdf(&path, &tmp)).await.unwrap_or_else(|_| Err("Conversion failed".into()));
            w.set_busy(false, "");
            match result {
                Ok(pdf) => w.add_pdf(pdf, title),
                Err(e) => w.error("Can't Open File", &e),
            }
        }));
    }

    fn add_pdf(self: &Rc<Self>, pdf: PathBuf, title: String) {
        let pages = match Source::open(&pdf) {
            Ok(s) => s.n_pages(),
            Err(e) => return self.error("Can't Open File", &format!("{title}: {e}")),
        };
        if pages == 0 {
            return self.error("Can't Open File", &format!("{title} has no pages."));
        }
        let first = self.entries.borrow().is_empty();
        self.entries.borrow_mut().push(Entry { pdf, title: title.clone(), pages });
        if !first {
            self.toast(&format!("Added “{title}”"));
        }
        self.reload_docs();
    }

    fn remove_doc(self: &Rc<Self>, index: usize) {
        if index >= self.entries.borrow().len() {
            return;
        }
        let entry = self.entries.borrow_mut().remove(index);
        self.clear_ranges();
        self.toast(&format!("Removed “{}”", entry.title));
        self.reload_docs();
    }

    fn move_doc(self: &Rc<Self>, from: usize, to: usize) {
        let n = self.entries.borrow().len();
        if from >= n || to >= n || from == to {
            return;
        }
        let entry = self.entries.borrow_mut().remove(from);
        self.entries.borrow_mut().insert(to, entry);
        self.clear_ranges();
        self.reload_docs();
    }

    /// Page numbers shift when documents move, so an old selection would print the wrong pages.
    fn clear_ranges(&self) {
        if !self.range_row.text().is_empty() {
            self.range_row.set_text("");
        }
    }

    /// Make the document to print from the entries: the only one, or all of them joined.
    fn reload_docs(self: &Rc<Self>) {
        let generation = self.doc_generation.get() + 1;
        self.doc_generation.set(generation);
        self.rebuild_doc_rows();
        let (paths, titles, doc_pages): (Vec<PathBuf>, Vec<String>, Vec<usize>) = {
            let entries = self.entries.borrow();
            (entries.iter().map(|e| e.pdf.clone()).collect(), entries.iter().map(|e| e.title.clone()).collect(), entries.iter().map(|e| e.pages).collect())
        };
        let title = match titles.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [first, rest @ ..] => format!("{first} + {}", plural(rest.len(), "more", "more")),
        };
        if paths.is_empty() {
            self.doc.replace(None);
            self.window.set_title(Some("Print Studio"));
            self.pages.clear();
            self.schedule();
            return;
        }
        if paths.len() == 1 {
            return self.set_doc(paths[0].clone(), title, doc_pages);
        }
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            w.set_busy(true, "Joining documents…");
            let dest = w.tmp.join(format!("joined-{generation}.pdf"));
            let out = dest.clone();
            let result = gio::spawn_blocking(move || pdfout::merge_files(&paths, &out)).await.unwrap_or_else(|_| Err("Joining the documents failed".into()));
            w.set_busy(false, "");
            if w.doc_generation.get() != generation {
                return; // the documents changed again meanwhile
            }
            match result {
                Ok(()) => w.set_doc(dest, title, doc_pages),
                Err(e) => w.error("Can't Join the Documents", &e),
            }
        }));
    }

    fn set_doc(self: &Rc<Self>, path: PathBuf, title: String, doc_pages: Vec<usize>) {
        let source = match Source::open(&path) {
            Ok(s) => Rc::new(s),
            Err(e) => return self.error("Can't Open File", &format!("{title}: {e}")),
        };
        self.window.set_title(Some(&format!("{title} — Print Studio")));
        let n = source.n_pages();
        let labels: Vec<String> = if doc_pages.len() > 1 {
            // "13" with the document and its own page number underneath.
            let entries = self.entries.borrow();
            let mut labels = Vec::with_capacity(n);
            for e in entries.iter() {
                let stem = Path::new(&e.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                for p in 1..=e.pages {
                    labels.push(format!("{}\t{stem} · {p}", labels.len() + 1));
                }
            }
            labels
        } else {
            (1..=n).map(|i| i.to_string()).collect()
        };
        let (s1, s2) = (source.clone(), source.clone());
        self.pages.set_items(
            &labels,
            Box::new(move |pos, px| {
                let side = vec![pos as usize];
                render_sheet(&s1, &SheetSpec::page(&side), px)
            }),
            Box::new(move |pos| s2.sizes[pos as usize]),
        );
        self.doc.replace(Some(Doc { path, title, source, doc_pages }));
        self.select_from_ranges(&self.range_row.text());
        self.schedule();
    }

    /// One row per document, draggable to reorder.
    fn rebuild_doc_rows(self: &Rc<Self>) {
        for row in self.doc_rows.take() {
            self.docs_group.remove(&row);
        }
        if self.separate_row.ancestor(adw::PreferencesGroup::static_type()).is_some() {
            self.docs_group.remove(&self.separate_row);
        }
        let entries = self.entries.borrow();
        let several = entries.len() > 1;
        self.docs_group.set_visible(!entries.is_empty());
        self.docs_group.set_description(several.then_some("Printed in this order. Drag to reorder."));
        let mut rows = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&glib::markup_escape_text(&entry.title));
            row.set_subtitle(&plural(entry.pages, "page", "pages"));
            row.set_title_lines(1);
            if several {
                let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
                handle.add_css_class("dim-label");
                row.add_prefix(&handle);
                row.add_css_class("doc-row");
                let source = gtk::DragSource::new();
                source.set_actions(gdk::DragAction::MOVE);
                source.set_content(Some(&gdk::ContentProvider::for_value(&(i as u32).to_value())));
                source.connect_drag_begin(clone!(#[weak] row, move |source, _| {
                    let icon = gtk::WidgetPaintable::new(Some(&row));
                    source.set_icon(Some(&icon), 0, 0);
                }));
                row.add_controller(source);
                let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
                target.connect_drop(clone!(#[weak(rename_to = w)] self, #[upgrade_or] false, move |_, value, _, _| {
                    let Ok(from) = value.get::<u32>() else { return false };
                    let w2 = w.clone();
                    // Rebuilding the rows inside the drop handler would destroy this target mid-signal.
                    glib::idle_add_local_once(move || w2.move_doc(from as usize, i));
                    true
                }));
                row.add_controller(target);
            }
            let remove = icon_button("user-trash-symbolic", "Remove from this job");
            remove.connect_clicked(clone!(#[weak(rename_to = w)] self, move |_| {
                let w2 = w.clone();
                glib::idle_add_local_once(move || w2.remove_doc(i));
            }));
            row.add_suffix(&remove);
            self.docs_group.add(&row);
            rows.push(row);
        }
        if several {
            self.docs_group.add(&self.separate_row);
        }
        self.doc_rows.replace(rows);
    }

    // New jobs while this window is open

    fn ask_incoming(self: &Rc<Self>, item: Incoming) {
        self.incoming.borrow_mut().push_back(item);
        self.window.present();
        if !self.asking.replace(true) {
            glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
                loop {
                    let next = w.incoming.borrow_mut().pop_front();
                    let Some(item) = next else { break };
                    w.ask_one(item).await;
                }
                w.asking.set(false);
            }));
        }
    }

    async fn ask_one(self: &Rc<Self>, item: Incoming) {
        let title = item.title();
        let pages = match &item {
            Incoming::Spool(job) => Source::open(&job.pdf).ok().map(|s| s.n_pages()),
            Incoming::File(path) if convert::is_pdf(path) => Source::open(path).ok().map(|s| s.n_pages()),
            Incoming::File(_) => None,
        };
        let what = match pages {
            Some(n) => format!("“{}” ({})", glib::markup_escape_text(&title), plural(n, "page", "pages")),
            None => format!("“{}”", glib::markup_escape_text(&title)),
        };
        let body = format!("{what} just arrived. Add it to the job you have open here, so they print together, or open it in its own window?");
        let dialog = adw::AlertDialog::new(Some("New Print Job"), Some(&body));
        dialog.set_body_use_markup(true);
        dialog.add_responses(&[("separate", "Open Separately"), ("add", "Add to This Job")]);
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("separate");
        if dialog.choose_future(Some(&self.window)).await == "add" {
            self.take(item);
        } else {
            Win::new(&self.app, Some(item)).present();
        }
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
        let passes = pipeline::plan_docs(&doc.doc_pages, &job)?;
        let sheets = pipeline::flatten(&passes, &job);
        let labels = pipeline::side_labels(&passes, job.two_sided());
        let first = pipeline::first_page(sheets.iter().map(|(s, _)| s)).unwrap_or(0);
        let fallback = doc.source.sizes.get(first).copied().unwrap_or((612.0, 792.0));
        let layout = SheetLayout::new(&job, self.paper(), self.margin());
        // Booklet spreads lie sideways on the paper (top edge to the left); turn them upright.
        let view = if job.booklet { sheets.iter().map(|(s, _)| (s.clone(), (job.rotate + 90) % 360)).collect() } else { sheets.clone() };
        Ok(Output { job, passes, sheets, view, labels, layout, fallback })
    }

    fn rebuild(self: &Rc<Self>) {
        let job = self.read_job();
        // Booklets choose their own layout; n-up layouts always fit the pages into their cells.
        self.nup_row.set_sensitive(!job.booklet);
        self.nup_row.set_subtitle(if job.booklet { "2, side by side, while making a booklet" } else { "" });
        self.page_set_row.set_sensitive(!job.booklet);
        self.orientation_row.set_sensitive(job.nup == 1 && !job.booklet);
        self.scaling_row.set_sensitive(job.nup == 1 && !job.booklet);

        let source = self.doc.borrow().as_ref().map(|d| d.source.clone());
        let Some(source) = source else {
            self.preview.clear();
            self.output.replace(None);
            self.body.set_visible_child_name(if self.busy.get() { "loaded" } else { "empty" });
            self.switcher.set_visible(false);
            self.sheets_label.set_text("No document");
            self.detail_label.set_text("Open a file, drop one here, or print to “PrintStudio” from any app");
            self.print_button.set_sensitive(false);
            self.update_info();
            return;
        };
        self.body.set_visible_child_name("loaded");
        self.switcher.set_visible(true);
        let out = match self.compute() {
            Ok(o) => o,
            Err(e) => {
                self.preview.clear();
                self.output.replace(None);
                self.sheets_label.set_text("Nothing to print");
                self.detail_label.set_text("");
                self.show_error(&e);
                self.print_button.set_sensitive(false);
                self.update_info();
                return;
            }
        };
        self.show_error("");
        self.print_button.set_sensitive(!self.printers.borrow().is_empty() && !self.busy.get());

        let sheets = Rc::new(out.view.clone());
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

        let paper = out.paper();
        self.sheets_label.set_text(&plural(paper, "sheet of paper", "sheets of paper"));
        let mut detail = if out.job.booklet {
            let booklets = out.job.booklet_sheets as usize;
            if booklets > 0 && paper > booklets {
                format!("{} to fold · printed in two passes", plural(paper.div_ceil(booklets), "booklet", "booklets"))
            } else {
                "One booklet to fold · printed in two passes".to_string()
            }
        } else if out.job.duplex {
            "Printed in two passes: fronts, then backs".to_string()
        } else {
            plural(out.page_count(), "page", "pages")
        };
        let saved = out.page_count().saturating_sub(paper);
        if saved > 0 {
            detail += &format!(" · saves {}", plural(saved, "sheet", "sheets"));
        }
        self.detail_label.set_text(&detail);
        self.output.replace(Some(out));
        self.update_info();
    }

    /// The line under the thumbnails.
    fn update_info(&self) {
        let doc = self.doc.borrow();
        let Some(doc) = doc.as_ref() else {
            self.info.set_text("");
            return;
        };
        let pages = plural(doc.source.n_pages(), "page", "pages");
        let docs = if doc.doc_pages.len() > 1 { format!("{} · ", plural(doc.doc_pages.len(), "document", "documents")) } else { String::new() };
        let text = if self.stack.visible_child_name().as_deref() == Some("pages") {
            format!("{} · {docs}{pages}", doc.title)
        } else {
            match self.output.borrow().as_ref() {
                Some(out) if out.job.booklet => format!("{} · booklet spreads, fronts then backs", doc.title),
                Some(out) if out.job.two_sided() => format!("{} · all fronts, then all backs, in print order", doc.title),
                Some(_) => format!("{} · sheets in print order", doc.title),
                None => doc.title.clone(),
            }
        };
        self.info.set_text(&text);
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
        let sheets = Rc::new(out.view);
        let (layout, fallback) = (out.layout, out.fallback);
        let (s1, sh1, s2, sh2) = (source.clone(), sheets.clone(), source, sheets.clone());
        viewer::open(
            &self.window,
            &format!("Print Preview — {title}"),
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
            out.labels.iter().map(|l| l.replace('\t', " · ")).collect(),
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
        let (Some(printer), Some(out), Some((src_path, title))) =
            (self.printer_name(), self.compute().ok(), self.doc.borrow().as_ref().map(|d| (d.path.clone(), d.title.clone())))
        else {
            return;
        };
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
                    return w.error("Print Failed", &e);
                }
            };
            if !out.job.two_sided() {
                w.set_busy(true, "Sending to printer…");
                let result = submit(&printer, &files[0], &title, &options, ready).await;
                w.set_busy(false, "");
                match result {
                    Ok(_) => w.window.close(),
                    Err(e) => w.error("Print Failed", &e),
                }
                return;
            }
            w.set_busy(false, "");
            if !w.duplex_flow(&printer, &files, &title, &options, out.paper(), ready).await {
                return;
            }
            if out.job.booklet {
                w.fold_instructions(&out).await;
            }
            w.window.close();
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
                        w.toast("Calibration printed. Check both sheets and adjust the two-sided options if needed.");
                    }
                }
                Err(e) => w.error("Print Failed", &e),
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
                self.error("Print Failed", &format!("Couldn't print the front sides:\n{e}"));
                return false;
            }
        };

        let dialog = self.reload_dialog(sheets, job_id);
        if dialog.choose_future(Some(&self.window)).await != "back" {
            return false;
        }
        self.set_busy(true, "Sending back sides…");
        let back = submit(printer, &files[1], &format!("{title} (backs)"), options, ready).await;
        self.set_busy(false, "");
        match back {
            Ok(_) => true,
            Err(e) => {
                self.error("Print Failed", &format!("Couldn't print the back sides:\n{e}"));
                false
            }
        }
    }

    /// "Reload the stack" instructions, with the print job's progress.
    fn reload_dialog(&self, sheets: usize, job_id: i32) -> adw::AlertDialog {
        let note = self.note_row.text().trim().to_string();
        let body = if note.is_empty() { FIRST_TIME } else { "" };
        let dialog = adw::AlertDialog::new(Some(&format!("Printing Front Sides ({})", plural(sheets, "sheet", "sheets"))), Some(body));
        dialog.set_body_use_markup(true);
        dialog.add_responses(&[("cancel", "Cancel"), ("back", "Print Back Sides")]);
        dialog.set_response_appearance("back", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("back"));
        dialog.set_close_response("cancel");

        let extra = gtk::Box::new(gtk::Orientation::Vertical, 12);
        extra.append(&art::reload());
        let steps = gtk::Grid::builder().column_spacing(10).row_spacing(6).halign(gtk::Align::Center).build();
        for (i, step) in INSTRUCTIONS.iter().enumerate() {
            let number = gtk::Label::new(Some(&(i + 1).to_string()));
            number.add_css_class("step-number");
            number.set_valign(gtk::Align::Start);
            let text = gtk::Label::new(None);
            text.set_markup(step);
            text.set_wrap(true);
            text.set_xalign(0.0);
            text.set_max_width_chars(40);
            steps.attach(&number, 0, i as i32, 1, 1);
            steps.attach(&text, 1, i as i32, 1, 1);
        }
        extra.append(&steps);
        if !note.is_empty() {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
            card.add_css_class("card");
            card.add_css_class("note-card");
            let heading = gtk::Label::new(Some("Your note for this printer"));
            heading.add_css_class("caption-heading");
            heading.add_css_class("accent");
            heading.set_xalign(0.0);
            let text = gtk::Label::new(Some(&note));
            text.set_wrap(true);
            text.set_xalign(0.0);
            card.append(&heading);
            card.append(&text);
            extra.append(&card);
        }
        let progress = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        progress.set_halign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        let status = gtk::Label::new(Some(&format!("Job {job_id}: queued…")));
        status.add_css_class("dim-label");
        status.set_wrap(true);
        progress.append(&spinner);
        progress.append(&status);
        extra.append(&progress);
        dialog.set_extra_child(Some(&extra));
        let (weak_status, weak_spinner) = (status.downgrade(), spinner.downgrade());
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let (Some(status), Some(spinner)) = (weak_status.upgrade(), weak_spinner.upgrade()) else { return glib::ControlFlow::Break };
            let state = cups::job_state(job_id);
            status.set_text(&match state {
                cups::JobState::Done => format!("✔ Job {job_id}: front sides printed. Reload the stack, then print the back sides."),
                cups::JobState::Printing => format!("Job {job_id}: printing front sides…"),
                s => format!("Job {job_id}: {}", s.describe()),
            });
            if state.is_final() {
                spinner.set_spinning(false);
                spinner.set_visible(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        dialog
    }

    /// After a booklet's backs are sent: how to fold it.
    async fn fold_instructions(self: &Rc<Self>, out: &Output) {
        let per = out.job.booklet_sheets as usize;
        let paper = out.paper() / out.job.copies.max(1) as usize;
        let booklets = if per == 0 { 1 } else { paper.div_ceil(per) };
        let mut body = String::from(
            "When the back sides have printed, keep the sheets in the order they came out.\n\n\
             The sheet with <b>page 1</b> on it goes on the outside. ",
        );
        if booklets > 1 {
            body += &format!(
                "The stack makes <b>{booklets} booklets of {per} sheet{}</b>: take them off {per} at a time, \
                 fold each group in half, then stack the folded booklets in order.",
                if per == 1 { "" } else { "s" }
            );
        } else {
            body += "Fold the whole stack in half along the middle, and staple it along the fold if you like.";
        }
        if out.job.copies > 1 {
            body += &format!("\n\nYou printed {} copies: each copy comes out as its own stack.", out.job.copies);
        }
        let dialog = adw::AlertDialog::new(Some("Fold Your Booklet"), Some(&body));
        dialog.set_body_use_markup(true);
        dialog.set_extra_child(Some(&art::fold(booklets)));
        dialog.add_response("done", "Done");
        dialog.set_default_response(Some("done"));
        dialog.set_close_response("done");
        dialog.choose_future(Some(&self.window)).await;
    }

    fn save_pdf(self: &Rc<Self>) {
        let (Some(out), Some((src_path, title))) = (self.compute().ok(), self.doc.borrow().as_ref().map(|d| (d.path.clone(), d.title.clone()))) else {
            return;
        };
        let stem = Path::new(&title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
        let suffix = if out.job.booklet { "booklet" } else { "print" };
        let dialog = gtk::FileDialog::builder().title("Save Output as PDF").initial_name(format!("{stem}-{suffix}.pdf")).build();
        glib::spawn_future_local(clone!(#[weak(rename_to = w)] self, async move {
            let Ok(file) = dialog.save_future(Some(&w.window)).await else { return };
            let Some(dest) = file.path() else { return };
            w.set_busy(true, "Saving…");
            let shown = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let result = gio::spawn_blocking(move || {
                let doc = pdfout::load(&src_path)?;
                pdfout::write_combined(&doc, &out.passes, &out.job, &out.layout, &dest)
            })
            .await
            .unwrap_or_else(|_| Err("Saving failed".into()));
            w.set_busy(false, "");
            match result {
                Ok(()) => w.toast(&format!("Saved “{shown}”")),
                Err(e) => w.error("Couldn't Save the PDF", &e),
            }
        }));
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Print Studio")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Khawar Zamman Wani")
            .copyright("© 2026 Khawar Zamman Wani")
            .license_type(gtk::License::MitX11)
            .comments("One print dialog for every app: reverse order, collate, n-up, booklets, manual two-sided printing and every option your printer offers.")
            .build();
        about.present(Some(&self.window));
    }

    // Feedback

    fn toast(&self, text: &str) {
        let toast = adw::Toast::new(&glib::markup_escape_text(text));
        toast.set_timeout(3);
        self.toasts.add_toast(toast);
    }

    /// A problem that stops printing, shown until it's fixed. Empty clears it.
    fn show_error(&self, text: &str) {
        self.banner.set_title(&glib::markup_escape_text(text));
        self.banner.set_revealed(!text.is_empty());
    }

    fn set_busy(self: &Rc<Self>, busy: bool, message: &str) {
        self.busy.set(busy);
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.print_button.set_sensitive(!busy && self.output.borrow().is_some());
        if busy {
            self.detail_label.set_text(message);
            if self.doc.borrow().is_none() {
                self.body.set_visible_child_name("loaded");
            }
        } else {
            self.schedule(); // brings back the paper summary
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
            "booklet" => self.booklet_row.set_enable_expansion(arg == "1"),
            "expand-booklet" => self.booklet_row.set_expanded(true),
            "sheets" => self.booklet_sheets_row.set_selected(BOOKLET_SHEETS.iter().position(|b| b.1.to_string() == arg).unwrap_or(0) as u32),
            "rtl" => self.binding_row.set_selected(if arg == "1" { 1 } else { 0 }),
            "gutter" => self.gutter_row.set_value(arg.parse().unwrap_or(0.0)),
            "copies" => self.copies_row.set_value(arg.parse().unwrap_or(1.0)),
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
            "print-state" => println!("scale={} range={:?} selected={:?} sheets={:?} detail={:?} info={:?} banner={:?} labels={:?}",
                self.window.scale_factor(), self.range_row.text(), self.pages.selected(), self.sheets_label.text(), self.detail_label.text(),
                self.info.text(), self.banner.is_revealed().then(|| self.banner.title()),
                self.output.borrow().as_ref().map(|o| o.labels.clone())),
            "print-docs" => println!("docs={:?} doc_pages={:?}", self.entries.borrow().iter().map(|e| (e.title.clone(), e.pages)).collect::<Vec<_>>(),
                self.doc.borrow().as_ref().map(|d| d.doc_pages.clone())),
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
            "open" | "add" => self.add_path(PathBuf::from(arg)),
            // A job arriving from outside, as `printstudio FILE` would deliver it.
            "incoming" => deliver(&self.app, Some(Incoming::File(PathBuf::from(arg)))),
            "remove" => self.remove_doc(arg.parse().unwrap_or(0)),
            "move" => {
                let (a, b) = arg.split_once(',').unwrap_or(("0", "1"));
                self.move_doc(a.parse().unwrap_or(0), b.parse().unwrap_or(1));
            }
            // Answer the dialog on screen, as if its button were clicked.
            "respond" => {
                if let Some(dialog) = self.window.visible_dialog().and_downcast::<adw::AlertDialog>() {
                    dialog.emit_by_name::<()>("response", &[&arg]);
                    dialog.force_close();
                }
            }
            "fold" => {
                if let Some(out) = self.output.borrow().clone() {
                    let w = self.clone();
                    glib::spawn_future_local(async move { w.fold_instructions(&out).await });
                }
            }
            // The reload instructions, without printing anything.
            "duplex-dialog" => self.reload_dialog(arg.parse().unwrap_or(3), 42).present(Some(&self.window)),
            "note" => self.note_row.set_text(arg),
            "about" => self.show_about(),
            "shortcuts" => super::show_shortcuts(self.window.upcast_ref()),
            "sidebar" => self.split.set_show_sidebar(arg == "1"),
            "resize" => {
                let (a, b) = arg.split_once('x').unwrap_or(("1240", "820"));
                self.window.set_default_size(a.parse().unwrap_or(1240), b.parse().unwrap_or(820));
            }
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
            "quit" => {
                for w in OPEN.with(|o| o.borrow().clone()) {
                    if let Some(dialog) = w.window.visible_dialog() {
                        dialog.force_close();
                    }
                    w.window.close();
                }
            }
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
