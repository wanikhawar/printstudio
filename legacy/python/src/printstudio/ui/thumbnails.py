"""A zoomable grid of PDF page thumbnails that only renders what is on screen."""

from __future__ import annotations

from PySide6.QtCore import QSize, Qt, QTimer, Signal
from PySide6.QtGui import QColor, QIcon, QImage, QPainter, QPixmap
from PySide6.QtPdf import QPdfDocument
from PySide6.QtWidgets import QListView, QListWidget, QListWidgetItem

PAGE_ROLE = Qt.UserRole
MIN_SIZE, MAX_SIZE = 90, 720


def framed(img, dpr: float = 1.0) -> QPixmap:
    """Draw a page with an outline and a soft shadow so white pages stand out."""
    w, h = img.width(), img.height()
    shadow = max(2, round(3 * dpr))
    pix = QPixmap(w + shadow + 1, h + shadow + 1)
    pix.fill(Qt.transparent)
    p = QPainter(pix)
    p.fillRect(shadow, shadow, w, h, QColor(0, 0, 0, 60))
    p.fillRect(0, 0, w, h, Qt.white)
    if not img.isNull():
        p.drawImage(0, 0, img)
    p.setPen(QColor(140, 140, 140))
    p.drawRect(0, 0, w - 1, h - 1)
    p.end()
    pix.setDevicePixelRatio(dpr)
    return pix


class ThumbnailList(QListWidget):
    zoomRequested = Signal(int)  # +1 bigger, -1 smaller
    pageActivated = Signal(int)  # double-click / Enter on a page

    def __init__(self, selectable: bool, parent=None):
        super().__init__(parent)
        self.setViewMode(QListView.IconMode)
        self.setResizeMode(QListView.Adjust)
        self.setMovement(QListView.Static)
        self.setUniformItemSizes(False)
        self.setSpacing(10)
        self.setWordWrap(True)
        if selectable:
            self.setSelectionMode(QListWidget.ExtendedSelection)
        else:
            self.setSelectionMode(QListWidget.NoSelection)
            self.setFocusPolicy(Qt.NoFocus)
        self.setStyleSheet(
            "QListWidget::item { border-radius: 6px; padding: 4px; }"
            "QListWidget::item:selected { background: palette(highlight); color: palette(highlighted-text); }"
        )
        self._doc: QPdfDocument | None = None
        self._size = 210
        self._rendered: set[int] = set()
        self._placeholders: dict[tuple[int, int], QPixmap] = {}
        self._timer = QTimer(self, singleShot=True, interval=20)
        self._timer.timeout.connect(self._render_visible)
        self.verticalScrollBar().valueChanged.connect(self._timer.start)
        self.itemActivated.connect(lambda item: self.pageActivated.emit(item.data(PAGE_ROLE)))
        self.setIconSize(QSize(self._size, self._size))

    @property
    def thumb_size(self) -> int:
        return self._size

    def set_document(self, doc: QPdfDocument | None, labels: list[str]) -> None:
        self.clear()
        self._doc = doc
        self._rendered.clear()
        if doc is None:
            return
        self.setUpdatesEnabled(False)
        for i, label in enumerate(labels):
            item = QListWidgetItem(QIcon(self._placeholder(i)), label)
            item.setData(PAGE_ROLE, i)
            item.setTextAlignment(Qt.AlignHCenter)
            self.addItem(item)
        self.setUpdatesEnabled(True)
        self._timer.start()

    def set_thumb_size(self, size: int) -> None:
        size = max(MIN_SIZE, min(MAX_SIZE, size))
        if size == self._size:
            return
        self._size = size
        self.setIconSize(QSize(size, size))
        self._placeholders.clear()
        self._rendered.clear()
        for row in range(self.count()):
            self.item(row).setIcon(QIcon(self._placeholder(row)))
        self._timer.start()

    def _page_pixels(self, i: int) -> tuple[int, int]:
        pt = self._doc.pagePointSize(i)
        w, h = max(pt.width(), 1), max(pt.height(), 1)
        scale = self._size / max(w, h)
        return max(1, int(w * scale)), max(1, int(h * scale))

    def _placeholder(self, i: int) -> QPixmap:
        w, h = self._page_pixels(i)
        if (w, h) not in self._placeholders:
            blank = QImage(w, h, QImage.Format_RGB32)
            blank.fill(Qt.white)
            self._placeholders[(w, h)] = framed(blank)
        return self._placeholders[(w, h)]

    def _render_visible(self) -> None:
        if self._doc is None or self._doc.status() != QPdfDocument.Status.Ready:
            return
        margin = self._size
        area = self.viewport().rect().adjusted(0, -margin, 0, margin)
        dpr = self.devicePixelRatioF()
        for row in range(self.count()):
            if row in self._rendered:
                continue
            item = self.item(row)
            if not self.visualItemRect(item).intersects(area):
                continue
            w, h = self._page_pixels(row)
            img = self._doc.render(row, QSize(int(w * dpr), int(h * dpr)))
            item.setIcon(QIcon(framed(img, dpr)))
            self._rendered.add(row)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._timer.start()

    def showEvent(self, event):
        super().showEvent(event)
        self._timer.start()

    def wheelEvent(self, event):
        if event.modifiers() & Qt.ControlModifier:
            self.zoomRequested.emit(1 if event.angleDelta().y() > 0 else -1)
            event.accept()
            return
        super().wheelEvent(event)
