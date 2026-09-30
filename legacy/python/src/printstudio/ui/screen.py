"""Window sizing that respects the screen the window opens on."""

from __future__ import annotations

from PySide6.QtGui import QGuiApplication
from PySide6.QtWidgets import QWidget


def fit_to_screen(window: QWidget, width: int, height: int, fraction: float = 0.85) -> None:
    """Resize to width x height, but never beyond ``fraction`` of the free screen area.

    Uses logical pixels, so display scaling (e.g. 1080p at 1.25x = 864 px tall)
    and panels/bars are taken into account.
    """
    anchor = window.parentWidget() or window
    screen = anchor.screen() or QGuiApplication.primaryScreen()
    if screen is None:
        window.resize(width, height)
        return
    free = screen.availableGeometry()
    window.resize(min(width, int(free.width() * fraction)),
                  min(height, int(free.height() * fraction)))
