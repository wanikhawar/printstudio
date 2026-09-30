"""The Print Studio window: options on the right, a preview of the actual output on the left."""

from __future__ import annotations

import io
import tempfile
from pathlib import Path

from pypdf import PdfReader, PdfWriter
from pypdf.errors import PdfReadError
from PySide6.QtCore import Qt, QTimer
from PySide6.QtGui import QKeySequence, QShortcut
from PySide6.QtPdf import QPdfDocument
from PySide6.QtWidgets import (
    QApplication, QCheckBox, QComboBox, QDialog, QFileDialog, QFormLayout, QGroupBox,
    QHBoxLayout, QInputDialog, QLabel, QLineEdit, QMainWindow, QMessageBox, QPushButton,
    QScrollArea, QSlider, QSpinBox, QSplitter, QTabWidget, QToolButton, QVBoxLayout, QWidget,
)

from .. import convert, pdfgen, printer
from ..config import Config
from ..pipeline import (
    NUP_LAYOUTS, JobOptions, Pass, describe_side, format_ranges, parse_ranges, plan, render_pass,
)
from ..spool import SpoolJob
from .duplex_dialog import DuplexDialog
from .page_viewer import PageViewer
from .screen import fit_to_screen
from .thumbnails import MAX_SIZE, MIN_SIZE, PAGE_ROLE, ThumbnailList

ZOOM_STEP = 1.2
TAB_PREVIEW, TAB_PAGES = 0, 1
OPEN_FILTER = (
    "Documents (*.pdf *.png *.jpg *.jpeg *.bmp *.gif *.tif *.tiff *.webp "
    "*.odt *.ods *.odp *.doc *.docx *.xls *.xlsx *.ppt *.pptx *.rtf *.txt);;All files (*)"
)


def _set_combo(combo: QComboBox, value) -> None:
    i = combo.findData(value)
    if i >= 0:
        combo.setCurrentIndex(i)


class MainWindow(QMainWindow):
    def __init__(self, cfg: Config, conn, path: Path | None = None,
                 spool_job: SpoolJob | None = None):
        super().__init__()
        self.cfg = cfg
        self.conn = conn
        self.spool_job = spool_job
        self._tmp = tempfile.TemporaryDirectory(prefix="printstudio-")
        self._reader: PdfReader | None = None
        self._title = ""
        self._info: printer.PrinterInfo | None = None
        self._ppd_widgets: dict[str, QComboBox] = {}
        self._preview_doc = QPdfDocument(self)
        self._preview_path: Path | None = None
        self._preview_labels: list[str] = []
        self._preview_serial = 0
        self._source_doc = QPdfDocument(self)
        self._source_path: Path | None = None
        self._syncing_selection = False
        self._rebuild_timer = QTimer(self, singleShot=True, interval=250)
        self._rebuild_timer.timeout.connect(self._rebuild_preview)

        self.setWindowTitle("Print Studio")
        fit_to_screen(self, 1180, 800, fraction=0.9)
        self.setAcceptDrops(True)
        self._build_ui()
        self._load_printers()

        if spool_job:
            self._load_pdf(spool_job.pdf, spool_job.title or "Untitled")
            self.copies.setValue(spool_job.copies)
        elif path:
            self._open_path(path)
        self._schedule()

    # UI construction

    def _build_ui(self):
        splitter = QSplitter()
        self.setCentralWidget(splitter)

        left = QWidget()
        lv = QVBoxLayout(left)
        self.doc_label = QLabel()
        self.doc_label.setWordWrap(True)
        lv.addWidget(self.doc_label)

        self.tabs = QTabWidget()
        self.preview = ThumbnailList(selectable=False)
        self.preview.setToolTip("Exactly what will print, in print order. Double-click to zoom in.")
        self.preview.pageActivated.connect(self._view_preview_page)
        self.pages = ThumbnailList(selectable=True)
        self.pages.setToolTip("Click to choose pages; Ctrl+click and Shift+click to choose several. "
                              "Double-click to zoom in.")
        self.pages.itemSelectionChanged.connect(self._on_page_selection)
        self.pages.pageActivated.connect(self._view_source_page)
        pages_tab = QWidget()
        pv = QVBoxLayout(pages_tab)
        pv.setContentsMargins(0, 0, 0, 0)
        sel_bar = QHBoxLayout()
        self.selection_label = QLabel()
        select_all = QPushButton("Select all")
        select_all.clicked.connect(self.pages.selectAll)
        clear = QPushButton("Clear")
        clear.setToolTip("No selection means every page prints")
        clear.clicked.connect(self.pages.clearSelection)
        invert = QPushButton("Invert")
        invert.clicked.connect(self._invert_selection)
        sel_bar.addWidget(self.selection_label, 1)
        for b in (select_all, clear, invert):
            sel_bar.addWidget(b)
        pv.addLayout(sel_bar)
        pv.addWidget(self.pages, 1)
        self.tabs.addTab(self.preview, "Print preview")
        self.tabs.addTab(pages_tab, "Select pages")
        lv.addWidget(self.tabs, 1)

        bottom = QHBoxLayout()
        self.summary = QLabel()
        self.summary.setWordWrap(True)
        bottom.addWidget(self.summary, 1)
        zoom_out = QToolButton(text="−", toolTip="Smaller thumbnails (Ctrl+−, or Ctrl+scroll)")
        zoom_out.clicked.connect(lambda: self._zoom(-1))
        self.zoom_slider = QSlider(Qt.Horizontal, minimum=MIN_SIZE, maximum=MAX_SIZE)
        self.zoom_slider.setFixedWidth(140)
        self.zoom_slider.setToolTip("Thumbnail size")
        self.zoom_slider.valueChanged.connect(self._set_thumb_size)
        zoom_in = QToolButton(text="+", toolTip="Bigger thumbnails (Ctrl++, or Ctrl+scroll)")
        zoom_in.clicked.connect(lambda: self._zoom(1))
        for w in (zoom_out, self.zoom_slider, zoom_in):
            bottom.addWidget(w)
        lv.addLayout(bottom)
        for view in (self.preview, self.pages):
            view.zoomRequested.connect(self._zoom)
        ui = self.cfg.ui
        self.zoom_slider.setValue(int(ui.get("thumb_size", 210)))
        self.tabs.setCurrentIndex(TAB_PAGES if ui.get("tab") == "pages" else TAB_PREVIEW)
        splitter.addWidget(left)

        right = QWidget()
        rv = QVBoxLayout(right)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        right.setMinimumWidth(400)
        form_host = QWidget()
        self.form = QVBoxLayout(form_host)
        scroll.setWidget(form_host)
        rv.addWidget(scroll, 1)

        self._build_document_group()
        self._build_printer_group()
        self._build_layout_group()
        self._build_duplex_group()
        self._ppd_host = QVBoxLayout()
        self.form.addLayout(self._ppd_host)
        self.form.addStretch(1)

        self.status = QLabel()
        self.status.setWordWrap(True)
        rv.addWidget(self.status)
        buttons = QHBoxLayout()
        save_pdf = QPushButton("Save as PDF…")
        save_pdf.clicked.connect(self._save_pdf)
        cancel = QPushButton("Cancel")
        cancel.clicked.connect(self.close)
        self.print_btn = QPushButton("Print")
        self.print_btn.setDefault(True)
        self.print_btn.clicked.connect(self._print)
        buttons.addWidget(save_pdf)
        buttons.addStretch(1)
        buttons.addWidget(cancel)
        buttons.addWidget(self.print_btn)
        rv.addLayout(buttons)

        splitter.addWidget(right)
        splitter.setStretchFactor(0, 1)
        splitter.setSizes([760, 420])

        QShortcut(QKeySequence.Print, self, self._print)
        QShortcut(QKeySequence.ZoomIn, self, lambda: self._zoom(1))
        QShortcut(QKeySequence("Ctrl+="), self, lambda: self._zoom(1))
        QShortcut(QKeySequence.ZoomOut, self, lambda: self._zoom(-1))
        QShortcut(QKeySequence.Open, self, self._open_dialog)
        QShortcut(QKeySequence(Qt.Key_Escape), self, self._escape)

    def _build_document_group(self):
        box = QGroupBox("Document")
        row = QHBoxLayout(box)
        self.file_label = QLabel("No document")
        self.file_label.setWordWrap(True)
        open_btn = QPushButton("Open…")
        open_btn.clicked.connect(self._open_dialog)
        row.addWidget(self.file_label, 1)
        row.addWidget(open_btn)
        self.form.addWidget(box)

    def _build_printer_group(self):
        box = QGroupBox("Printer")
        f = QFormLayout(box)
        self.printer_combo = QComboBox()
        self.printer_combo.currentTextChanged.connect(self._on_printer_changed)
        f.addRow("Printer:", self.printer_combo)

        row = QHBoxLayout()
        self.preset_combo = QComboBox()
        self.preset_combo.activated.connect(self._apply_preset)
        save = QPushButton("Save…")
        save.setToolTip("Save the current settings as a preset")
        save.clicked.connect(self._save_preset)
        delete = QPushButton("Delete")
        delete.clicked.connect(self._delete_preset)
        row.addWidget(self.preset_combo, 1)
        row.addWidget(save)
        row.addWidget(delete)
        f.addRow("Preset:", row)
        self.form.addWidget(box)
        self._refresh_presets()

    def _build_layout_group(self):
        box = QGroupBox("Pages")
        f = QFormLayout(box)
        self.range_edit = QLineEdit()
        self.range_edit.setPlaceholderText("All pages, e.g. 1-3, 7, 10-")
        self.range_edit.textChanged.connect(self._on_range_edited)
        f.addRow("Pages:", self.range_edit)

        self.page_set = QComboBox()
        for text, value in (("All pages", "all"), ("Odd pages only", "odd"), ("Even pages only", "even")):
            self.page_set.addItem(text, value)
        self.page_set.currentIndexChanged.connect(self._schedule)
        f.addRow("Print:", self.page_set)

        self.copies = QSpinBox(minimum=1, maximum=999)
        self.copies.valueChanged.connect(self._schedule)
        self.collate = QCheckBox("Collate")
        self.collate.setToolTip("On: 1,2,3, 1,2,3. Off: 1,1, 2,2, 3,3")
        self.collate.toggled.connect(self._schedule)
        row = QHBoxLayout()
        row.addWidget(self.copies)
        row.addWidget(self.collate)
        row.addStretch(1)
        f.addRow("Copies:", row)

        self.reverse = QCheckBox("Reverse order (last page first)")
        self.reverse.setToolTip("Use this when the printer stacks pages face up, so the "
                                "finished stack comes out in the right order.")
        self.reverse.toggled.connect(self._schedule)
        f.addRow("", self.reverse)

        self.nup = QComboBox()
        for n in NUP_LAYOUTS:
            self.nup.addItem(str(n), n)
        self.nup.currentIndexChanged.connect(self._schedule)
        f.addRow("Pages per sheet:", self.nup)

        self.rotate = QComboBox()
        for text, value in (("None", 0), ("90° clockwise", 90), ("180°", 180), ("90° counter-clockwise", 270)):
            self.rotate.addItem(text, value)
        self.rotate.currentIndexChanged.connect(self._schedule)
        f.addRow("Rotate:", self.rotate)

        self.fit = QCheckBox("Scale to fit paper")
        self.fit.toggled.connect(self._schedule)
        f.addRow("", self.fit)
        self.form.addWidget(box)

    def _build_duplex_group(self):
        self.duplex = QGroupBox("Manual two-sided printing")
        self.duplex.setCheckable(True)
        self.duplex.setChecked(False)
        self.duplex.toggled.connect(self._schedule)
        v = QVBoxLayout(self.duplex)
        hint = QLabel("Prints the front sides first, then walks you through reloading the "
                      "stack to print the backs.")
        hint.setWordWrap(True)
        v.addWidget(hint)
        self.backs_reverse = QCheckBox("Reverse order of back sides")
        self.backs_reverse.toggled.connect(self._schedule)
        self.backs_rotate = QCheckBox("Rotate back sides 180°")
        self.backs_rotate.toggled.connect(self._schedule)
        v.addWidget(self.backs_reverse)
        v.addWidget(self.backs_rotate)
        self.duplex_note = QLineEdit()
        self.duplex_note.setPlaceholderText("Note to yourself, e.g. 'printed side up, top edge in first'")
        v.addWidget(self.duplex_note)
        test = QPushButton("Print calibration test (2 sheets)")
        test.clicked.connect(self._print_calibration)
        v.addWidget(test)
        self.form.addWidget(self.duplex)

    def _build_ppd_widgets(self):
        while self._ppd_host.count():
            w = self._ppd_host.takeAt(0).widget()
            if w:
                w.deleteLater()
        self._ppd_widgets.clear()
        if not self._info or not self._info.options:
            return

        def combo_for(opt: printer.PrinterOption) -> QComboBox:
            c = QComboBox()
            for value, label in opt.choices:
                c.addItem(label, value)
            _set_combo(c, opt.default)
            c.currentIndexChanged.connect(self._schedule)
            self._ppd_widgets[opt.keyword] = c
            return c

        common = QGroupBox("Printer settings")
        cf = QFormLayout(common)
        for opt in (o for o in self._info.options if o.common):
            cf.addRow(opt.label + ":", combo_for(opt))
        self._ppd_host.addWidget(common)

        rest = [o for o in self._info.options if not o.common]
        if rest:
            toggle = QToolButton()
            toggle.setText("Advanced printer settings")
            toggle.setCheckable(True)
            toggle.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            toggle.setArrowType(Qt.RightArrow)
            advanced = QWidget()
            af = QFormLayout(advanced)
            group = None
            for opt in rest:
                if opt.group != group:
                    group = opt.group
                    af.addRow(QLabel(f"<b>{group}</b>"))
                af.addRow(opt.label + ":", combo_for(opt))
            advanced.setVisible(False)

            def on_toggle(checked):
                advanced.setVisible(checked)
                toggle.setArrowType(Qt.DownArrow if checked else Qt.RightArrow)

            toggle.toggled.connect(on_toggle)
            self._ppd_host.addWidget(toggle)
            self._ppd_host.addWidget(advanced)

    # Reading and applying settings

    def _read_job(self) -> JobOptions:
        return JobOptions(
            page_ranges=self.range_edit.text(),
            page_set=self.page_set.currentData(),
            copies=self.copies.value(),
            collate=self.collate.isChecked(),
            reverse=self.reverse.isChecked(),
            nup=self.nup.currentData(),
            rotate=self.rotate.currentData(),
            fit_to_page=self.fit.isChecked(),
            duplex=self.duplex.isChecked(),
            backs_reverse=self.backs_reverse.isChecked(),
            backs_rotate=self.backs_rotate.isChecked(),
        )

    def _apply_job(self, job: JobOptions):
        self.range_edit.setText(job.page_ranges)
        _set_combo(self.page_set, job.page_set)
        self.copies.setValue(job.copies)
        self.collate.setChecked(job.collate)
        self.reverse.setChecked(job.reverse)
        _set_combo(self.nup, job.nup)
        _set_combo(self.rotate, job.rotate)
        self.fit.setChecked(job.fit_to_page)
        self.duplex.setChecked(job.duplex)
        self.backs_reverse.setChecked(job.backs_reverse)
        self.backs_rotate.setChecked(job.backs_rotate)

    def _read_ppd(self) -> dict[str, str]:
        return {k: c.currentData() for k, c in self._ppd_widgets.items()}

    def _apply_ppd(self, values: dict[str, str]):
        for keyword, value in values.items():
            if keyword in self._ppd_widgets:
                _set_combo(self._ppd_widgets[keyword], value)

    def _paper(self) -> tuple[float, float] | None:
        if not self._info or "PageSize" not in self._ppd_widgets:
            return None
        return self._info.paper(self._ppd_widgets["PageSize"].currentData())

    # Printers and presets

    def _load_printers(self):
        names = printer.list_printers(self.conn)
        self.printer_combo.blockSignals(True)
        self.printer_combo.addItems(names)
        self.printer_combo.blockSignals(False)
        preferred = self.cfg.last_printer if self.cfg.last_printer in names else \
            printer.default_printer(self.conn, names)
        if preferred:
            self.printer_combo.setCurrentText(preferred)
            self._on_printer_changed(preferred)
        else:
            self._set_status("No printers found. Add one in your system's printer settings.", error=True)

    def _on_printer_changed(self, name: str):
        if not name:
            return
        current = self._read_job()
        try:
            self._info = printer.printer_info(self.conn, name)
        except Exception as e:
            self._info = printer.PrinterInfo(name)
            self._set_status(f"Couldn't read settings for {name}: {e}", error=True)
        self._build_ppd_widgets()
        job, ppd = self.cfg.printer_settings(name)
        job.page_ranges, job.copies = current.page_ranges, current.copies
        self._apply_job(job)
        self._apply_ppd(ppd)
        self.duplex_note.setText(self.cfg.printer_note(name))
        self._schedule()

    def _refresh_presets(self, select: str | None = None):
        self.preset_combo.clear()
        self.preset_combo.addItem("Choose a preset…", None)
        for name in self.cfg.preset_names():
            self.preset_combo.addItem(name, name)
        if select:
            _set_combo(self.preset_combo, select)

    def _apply_preset(self):
        name = self.preset_combo.currentData()
        if not name:
            return
        job_fields, ppd = self.cfg.preset(name)
        merged = self._read_job().to_dict() | job_fields
        self._apply_job(JobOptions.from_dict(merged))
        self._apply_ppd(ppd)
        self._set_status(f"Preset “{name}” applied.")

    def _save_preset(self):
        name, ok = QInputDialog.getText(self, "Save preset", "Preset name:",
                                        text=self.preset_combo.currentData() or "")
        name = name.strip()
        if not ok or not name:
            return
        self.cfg.set_preset(name, self._read_job(), self._read_ppd())
        self.cfg.save()
        self._refresh_presets(select=name)
        self._set_status(f"Preset “{name}” saved.")

    def _delete_preset(self):
        name = self.preset_combo.currentData()
        if not name:
            return
        if QMessageBox.question(self, "Delete preset", f"Delete preset “{name}”?") != QMessageBox.Yes:
            return
        self.cfg.delete_preset(name)
        self.cfg.save()
        self._refresh_presets()

    def _remember_printer_settings(self):
        name = self.printer_combo.currentText()
        if not name:
            return
        self.cfg.last_printer = name
        self.cfg.set_printer_settings(name, self._read_job(), self._read_ppd())
        self.cfg.set_printer_note(name, self.duplex_note.text().strip())
        self.cfg.save()

    # Documents

    def _open_dialog(self):
        path, _ = QFileDialog.getOpenFileName(self, "Open document", str(Path.home()), OPEN_FILTER)
        if path:
            self._open_path(Path(path))

    def _open_path(self, path: Path):
        QApplication.setOverrideCursor(Qt.WaitCursor)
        try:
            pdf = convert.to_pdf(path, Path(self._tmp.name))
        except convert.ConversionError as e:
            QApplication.restoreOverrideCursor()
            QMessageBox.warning(self, "Can't open file", str(e))
            return
        QApplication.restoreOverrideCursor()
        self._load_pdf(pdf, path.name)

    def _load_pdf(self, pdf: Path, title: str):
        try:
            reader = PdfReader(pdf)
            if reader.is_encrypted and not reader.decrypt(""):
                raise PdfReadError("the PDF is password protected")
            n = len(reader.pages)
        except Exception as e:
            QMessageBox.warning(self, "Can't open file", f"Couldn't read {title}: {e}")
            return
        self._reader = reader
        self._title = title
        self._source_path = pdf
        self._source_doc.close()
        self._source_doc.load(str(pdf))
        self.pages.set_document(self._source_doc, [str(i + 1) for i in range(n)])
        self._select_from_ranges(self.range_edit.text())
        self.file_label.setText(title)
        self.doc_label.setText(f"<b>{title}</b> · {n} page{'s' * (n != 1)}")
        self.setWindowTitle(f"{title} — Print Studio")
        self._schedule()

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()

    def dropEvent(self, event):
        urls = [u for u in event.mimeData().urls() if u.isLocalFile()]
        if urls:
            self._open_path(Path(urls[0].toLocalFile()))

    # Preview

    def _schedule(self, *_):
        self._rebuild_timer.start()

    def _build_passes(self) -> tuple[JobOptions, list[Pass]]:
        job = self._read_job()
        return job, plan(len(self._reader.pages), job)

    def _render_all(self, job: JobOptions, passes: list[Pass]) -> PdfWriter:
        writer = PdfWriter()
        for p in passes:
            render_pass(self._reader, p, job, self._paper(), writer)
        return writer

    def _rebuild_preview(self):
        self.preview.set_document(None, [])
        self._preview_doc.close()
        self._preview_labels = []
        if not self._reader:
            self.doc_label.setText("<b>No document.</b> Open a file, drop one here, or print "
                                   "to the “PrintStudio” printer from any app.")
            self.summary.clear()
            self.print_btn.setEnabled(False)
            return
        try:
            job, passes = self._build_passes()
        except ValueError as e:
            self._set_status(str(e), error=True)
            self.summary.clear()
            self.print_btn.setEnabled(False)
            return
        self.print_btn.setEnabled(self.printer_combo.count() > 0)
        self._set_status("")

        self._preview_serial += 1
        path = Path(self._tmp.name) / f"preview-{self._preview_serial % 2}.pdf"
        self._render_all(job, passes).write(path)
        self._preview_doc.load(str(path))
        self._preview_path = path

        for p in passes:
            for n, side in enumerate(p.sides, 1):
                prefix = f"{p.label} {n}" if job.duplex else f"{n}"
                self._preview_labels.append(f"{prefix} · {describe_side(side)}")
        self.preview.set_document(self._preview_doc, self._preview_labels)

        if job.duplex:
            sheets = len(passes[0].sides)
            self.summary.setText(f"{sheets} sheet{'s' * (sheets != 1)}, printed in two passes "
                                 "(fronts, then backs).")
        else:
            sheets = len(passes[0].sides)
            self.summary.setText(f"{sheets} sheet{'s' * (sheets != 1)} of paper. "
                                 "The preview shows the exact print order.")

    # Output

    def _cups_options(self, job: JobOptions) -> dict[str, str]:
        options = self._read_ppd()
        if job.fit_to_page:
            options["fit-to-page"] = "true"
        return options

    def _write_pass(self, job: JobOptions, p: Pass, name: str) -> str:
        path = Path(self._tmp.name) / name
        render_pass(self._reader, p, job, self._paper()).write(path)
        return str(path)

    def _print(self):
        if not self._reader or not self.print_btn.isEnabled():
            return
        name = self.printer_combo.currentText()
        try:
            job, passes = self._build_passes()
        except ValueError as e:
            self._set_status(str(e), error=True)
            return
        options = self._cups_options(job)
        title = self._title or "Print Studio"
        self._remember_printer_settings()

        if not job.duplex:
            path = self._write_pass(job, passes[0], "job.pdf")
            try:
                printer.submit(self.conn, name, path, title, options)
            except Exception as e:
                QMessageBox.critical(self, "Print failed", str(e))
                return
            self.close()
            return

        front = self._write_pass(job, passes[0], "front.pdf")
        back = self._write_pass(job, passes[1], "back.pdf")
        dlg = DuplexDialog(
            self, self.conn, name,
            lambda: printer.submit(self.conn, name, front, f"{title} (fronts)", options),
            lambda: printer.submit(self.conn, name, back, f"{title} (backs)", options),
            sheets=len(passes[0].sides), note=self.duplex_note.text().strip(),
        )
        if dlg.exec() == QDialog.Accepted:
            self.close()

    def _print_calibration(self):
        name = self.printer_combo.currentText()
        if not name:
            return
        current = self._read_job()
        job = JobOptions(reverse=current.reverse, duplex=True,
                         backs_reverse=current.backs_reverse, backs_rotate=current.backs_rotate)
        buf = io.BytesIO()
        pdfgen.duplex_test_pdf(self._paper() or pdfgen.A4).write(buf)
        reader = PdfReader(io.BytesIO(buf.getvalue()))
        passes = plan(len(reader.pages), job)
        paths = []
        for i, p in enumerate(passes):
            path = Path(self._tmp.name) / f"calibration-{i}.pdf"
            render_pass(reader, p, job).write(path)
            paths.append(str(path))
        options = self._read_ppd()
        self._remember_printer_settings()
        dlg = DuplexDialog(
            self, self.conn, name,
            lambda: printer.submit(self.conn, name, paths[0], "Two-sided calibration (fronts)", options),
            lambda: printer.submit(self.conn, name, paths[1], "Two-sided calibration (backs)", options),
            sheets=2, note=self.duplex_note.text().strip(),
        )
        if dlg.exec() == QDialog.Accepted:
            self._set_status("Calibration printed. Check both sheets and adjust the "
                             "two-sided options if needed.")

    def _save_pdf(self):
        if not self._reader:
            return
        try:
            job, passes = self._build_passes()
        except ValueError as e:
            self._set_status(str(e), error=True)
            return
        stem = Path(self._title).stem or "document"
        path, _ = QFileDialog.getSaveFileName(
            self, "Save output as PDF", str(Path.home() / f"{stem}-print.pdf"), "PDF (*.pdf)")
        if path:
            self._render_all(job, passes).write(path)
            self._set_status(f"Saved {path}")

    # Thumbnails: zoom, page selection, full-size viewer

    def _zoom(self, direction: int):
        size = self.zoom_slider.value()
        self.zoom_slider.setValue(round(size * ZOOM_STEP) if direction > 0 else round(size / ZOOM_STEP))

    def _set_thumb_size(self, size: int):
        self.preview.set_thumb_size(size)
        self.pages.set_thumb_size(size)

    def _on_page_selection(self):
        n = self.pages.count()
        rows = [item.data(PAGE_ROLE) for item in self.pages.selectedItems()]
        self._update_selection_label(len(rows), n)
        if self._syncing_selection:
            return
        text = "" if len(rows) in (0, n) else format_ranges(rows)
        self._syncing_selection = True
        self.range_edit.setText(text)
        self._syncing_selection = False

    def _on_range_edited(self, text: str):
        self._schedule()
        if not self._syncing_selection:
            self._select_from_ranges(text)

    def _select_from_ranges(self, text: str):
        n = self.pages.count()
        try:
            wanted = set(parse_ranges(text, n)) if text.strip() else set()
        except ValueError:
            return  # half-typed range; keep the current selection
        self._syncing_selection = True
        self.pages.blockSignals(True)
        for row in range(n):
            self.pages.item(row).setSelected(row in wanted)
        self.pages.blockSignals(False)
        self._syncing_selection = False
        self.pages.viewport().update()
        self._update_selection_label(len(wanted), n)

    def _invert_selection(self):
        self.pages.blockSignals(True)
        for row in range(self.pages.count()):
            item = self.pages.item(row)
            item.setSelected(not item.isSelected())
        self.pages.blockSignals(False)
        self._on_page_selection()

    def _update_selection_label(self, selected: int, total: int):
        if total == 0:
            self.selection_label.clear()
        elif selected in (0, total):
            self.selection_label.setText(f"All {total} pages will print")
        else:
            self.selection_label.setText(f"{selected} of {total} pages selected")

    def _view_preview_page(self, index: int):
        if self._preview_path:
            self._show_viewer(PageViewer(self, str(self._preview_path), index,
                                         f"Print preview — {self._title}", self._preview_labels))

    def _view_source_page(self, index: int):
        if self._source_path:
            self._show_viewer(PageViewer(self, str(self._source_path), index, self._title))

    def _escape(self):
        # If a viewer is open, Escape closes it rather than the whole app.
        viewers = [c for c in self.children() if isinstance(c, PageViewer) and c.isVisible()]
        if viewers:
            viewers[-1].reject()
        else:
            self.close()

    def _show_viewer(self, viewer: PageViewer):
        viewer.show()
        viewer.raise_()
        viewer.activateWindow()

    def _set_status(self, text: str, error: bool = False):
        self.status.setStyleSheet("color: #c0392b;" if error else "")
        self.status.setText(text)

    def closeEvent(self, event):
        # Re-read first: another Print Studio window may have saved since we loaded.
        fresh = Config(self.cfg.path)
        fresh.ui.update(thumb_size=self.zoom_slider.value(),
                        tab="pages" if self.tabs.currentIndex() == TAB_PAGES else "preview")
        fresh.save()
        self.preview.set_document(None, [])
        self.pages.set_document(None, [])
        self._preview_doc.close()
        self._source_doc.close()
        if self.spool_job:
            self.spool_job.remove()
        self._tmp.cleanup()
        super().closeEvent(event)
