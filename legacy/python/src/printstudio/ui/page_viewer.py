"""Full-size page viewer with zoom, opened by double-clicking a thumbnail."""

from __future__ import annotations

import shutil
import tempfile
from pathlib import Path

from PySide6.QtCore import QEvent, QPointF, Qt, QTimer
from PySide6.QtGui import QKeySequence, QShortcut
from PySide6.QtPdf import QPdfDocument
from PySide6.QtPdfWidgets import QPdfView
from PySide6.QtWidgets import QDialog, QHBoxLayout, QLabel, QPushButton, QSpinBox, QVBoxLayout

from .screen import fit_to_screen

ZOOM_STEP = 1.25
MIN_ZOOM, MAX_ZOOM = 0.1, 8.0


class PageViewer(QDialog):
    def __init__(self, parent, pdf_path: str, page: int, title: str, labels: list[str] | None = None):
        super().__init__(parent)
        self.setWindowTitle(title)
        fit_to_screen(self, 900, 1000)
        self.setAttribute(Qt.WA_DeleteOnClose)
        # Modal, so keys like Escape go to the viewer and not to the main window.
        self.setModal(True)
        self._labels = labels or []

        # Work on a private copy: the main window rewrites its preview file
        # whenever an option changes.
        self._tmp = tempfile.TemporaryDirectory(prefix="printstudio-view-")
        copy = Path(self._tmp.name) / "view.pdf"
        shutil.copyfile(pdf_path, copy)
        self._doc = QPdfDocument(self)
        self._doc.load(str(copy))

        self.view = QPdfView(self)
        self.view.setDocument(self._doc)
        self.view.setPageMode(QPdfView.PageMode.MultiPage)
        self.view.setPageSpacing(12)
        self.view.viewport().installEventFilter(self)

        bar = QHBoxLayout()
        self.page_spin = QSpinBox(minimum=1, maximum=max(1, self._doc.pageCount()))
        self.page_spin.setPrefix("Page ")
        self.page_spin.setSuffix(f" of {self._doc.pageCount()}")
        self.page_spin.valueChanged.connect(lambda v: self._jump(v - 1))
        self.page_label = QLabel()
        zoom_out = QPushButton("−")
        zoom_out.setToolTip("Zoom out (Ctrl+−)")
        zoom_out.clicked.connect(lambda: self._zoom_by(1 / ZOOM_STEP))
        self.zoom_label = QLabel()
        self.zoom_label.setMinimumWidth(48)
        self.zoom_label.setAlignment(Qt.AlignCenter)
        zoom_in = QPushButton("+")
        zoom_in.setToolTip("Zoom in (Ctrl++)")
        zoom_in.clicked.connect(lambda: self._zoom_by(ZOOM_STEP))
        fit_width = QPushButton("Fit width")
        fit_width.clicked.connect(lambda: self._set_mode(QPdfView.ZoomMode.FitToWidth))
        fit_page = QPushButton("Fit page")
        fit_page.clicked.connect(lambda: self._set_mode(QPdfView.ZoomMode.FitInView))
        actual = QPushButton("100%")
        actual.clicked.connect(lambda: self._set_zoom(1.0))
        for w in (zoom_out, zoom_in):
            w.setFixedWidth(34)
        bar.addWidget(self.page_spin)
        bar.addWidget(self.page_label)
        bar.addStretch(1)
        for w in (zoom_out, self.zoom_label, zoom_in, actual, fit_width, fit_page):
            bar.addWidget(w)
        bar.addSpacing(12)
        close = QPushButton("Close")
        close.setToolTip("Close the viewer (Esc)")
        close.clicked.connect(self.reject)
        bar.addWidget(close)

        layout = QVBoxLayout(self)
        layout.addLayout(bar)
        layout.addWidget(self.view, 1)

        QShortcut(QKeySequence.ZoomIn, self, lambda: self._zoom_by(ZOOM_STEP))
        QShortcut(QKeySequence("Ctrl+="), self, lambda: self._zoom_by(ZOOM_STEP))
        QShortcut(QKeySequence.ZoomOut, self, lambda: self._zoom_by(1 / ZOOM_STEP))
        QShortcut(QKeySequence("Ctrl+0"), self, lambda: self._set_mode(QPdfView.ZoomMode.FitInView))

        self.view.zoomFactorChanged.connect(self._update_zoom_label)
        self.view.pageNavigator().currentPageChanged.connect(self._on_page_changed)
        self._set_mode(QPdfView.ZoomMode.FitInView)
        self._start_page = page
        self._on_page_changed(page)

    def showEvent(self, event):
        super().showEvent(event)
        # QPdfView can only scroll to a page once it has been laid out.
        if self._start_page is not None:
            page, self._start_page = self._start_page, None
            QTimer.singleShot(0, lambda: self._jump(page))

    def _jump(self, page: int):
        if 0 <= page < self._doc.pageCount():
            self.view.pageNavigator().jump(page, QPointF(), self.view.pageNavigator().currentZoom())

    def _on_page_changed(self, page: int):
        self.page_spin.blockSignals(True)
        self.page_spin.setValue(page + 1)
        self.page_spin.blockSignals(False)
        self.page_label.setText(self._labels[page] if 0 <= page < len(self._labels) else "")

    def _current_zoom(self) -> float:
        """The zoom actually on screen. QPdfView doesn't report it in the fit modes."""
        mode = self.view.zoomMode()
        if mode == QPdfView.ZoomMode.Custom or self._doc.pageCount() == 0:
            return self.view.zoomFactor()
        size = self._doc.pagePointSize(self.view.pageNavigator().currentPage())
        px_per_pt = self.logicalDpiX() / 72
        vp = self.view.viewport()
        fit_w = (vp.width() - 24) / max(size.width() * px_per_pt, 1)
        if mode == QPdfView.ZoomMode.FitToWidth:
            return fit_w
        return min(fit_w, (vp.height() - 24) / max(size.height() * px_per_pt, 1))

    def _zoom_by(self, factor: float):
        self._set_zoom(self._current_zoom() * factor)

    def _set_zoom(self, zoom: float):
        spot = self._view_centre()
        self.view.setZoomMode(QPdfView.ZoomMode.Custom)
        self.view.setZoomFactor(max(MIN_ZOOM, min(MAX_ZOOM, zoom)))
        self._restore_centre(spot)

    def _set_mode(self, mode):
        spot = self._view_centre()
        self.view.setZoomMode(mode)
        self._restore_centre(spot)

    # The view keeps its pixel scroll offset across zoom changes, which lands
    # on a different page. Keep the same spot of the document centred instead.

    def _view_centre(self) -> float:
        sb = self.view.verticalScrollBar()
        return (sb.value() + sb.pageStep() / 2) / max(sb.maximum() + sb.pageStep(), 1)

    def _restore_centre(self, fraction: float):
        self._update_zoom_label()

        def apply():
            sb = self.view.verticalScrollBar()
            sb.setValue(round(fraction * (sb.maximum() + sb.pageStep()) - sb.pageStep() / 2))

        apply()
        QTimer.singleShot(0, apply)  # again once the new layout has settled

    def _update_zoom_label(self, *_):
        self.zoom_label.setText(f"{round(self._current_zoom() * 100)}%")

    def eventFilter(self, obj, event):
        if event.type() == QEvent.Wheel and event.modifiers() & Qt.ControlModifier:
            self._zoom_by(ZOOM_STEP if event.angleDelta().y() > 0 else 1 / ZOOM_STEP)
            return True
        if event.type() == QEvent.Resize and self.view.zoomMode() != QPdfView.ZoomMode.Custom:
            self._update_zoom_label()
        return super().eventFilter(obj, event)

    def done(self, result):
        # Escape, the Close button and the window manager all end up here.
        self._doc.close()
        self._tmp.cleanup()
        super().done(result)
