"""Guides the user between the two passes of a manual two-sided job."""

from __future__ import annotations

from PySide6.QtCore import QTimer
from PySide6.QtWidgets import (
    QDialog, QDialogButtonBox, QLabel, QMessageBox, QPushButton, QVBoxLayout,
)

from .. import printer

INSTRUCTIONS = """\
<ol>
<li>Wait until <b>all front sides</b> have finished printing.</li>
<li>Take the whole stack out of the output tray. <b>Don't reorder it.</b></li>
<li>Put it back in the paper tray so the <b>blank sides</b> get printed next.</li>
<li>Click <b>Print back sides</b>.</li>
</ol>
<p>First time with this printer? Use <i>Print calibration test</i> in the main
window to check how the stack has to go back in.</p>
"""


class DuplexDialog(QDialog):
    def __init__(self, parent, conn, printer_name: str, submit_front, submit_back,
                 sheets: int, note: str = ""):
        super().__init__(parent)
        self.setWindowTitle("Two-sided printing")
        self.setMinimumWidth(460)
        self._conn = conn
        self._submit_back = submit_back
        self._job_id: int | None = None

        layout = QVBoxLayout(self)
        heading = QLabel(f"<h3>Printing front sides ({sheets} sheet{'s' * (sheets != 1)})</h3>")
        layout.addWidget(heading)
        self._status = QLabel()
        layout.addWidget(self._status)
        body = QLabel(INSTRUCTIONS)
        body.setWordWrap(True)
        layout.addWidget(body)
        if note:
            note_label = QLabel(f"<b>Your note for {printer_name}:</b> {note}")
            note_label.setWordWrap(True)
            layout.addWidget(note_label)

        buttons = QDialogButtonBox()
        self._back_btn = QPushButton("Print back sides")
        self._back_btn.setDefault(True)
        buttons.addButton(self._back_btn, QDialogButtonBox.AcceptRole)
        buttons.addButton(QDialogButtonBox.Cancel)
        self._back_btn.clicked.connect(self._print_back)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)

        self._timer = QTimer(self, interval=1000)
        self._timer.timeout.connect(self._poll)
        try:
            self._job_id = submit_front()
        except Exception as e:
            QMessageBox.critical(parent, "Print failed", f"Could not print the front sides:\n{e}")
            QTimer.singleShot(0, self.reject)
            return
        self._poll()
        self._timer.start()

    def _poll(self):
        state = printer.job_state(self._conn, self._job_id)
        text = {
            "done": "✔ Front sides printed. Reload the stack, then print the back sides.",
            "queued": "Front sides queued…",
            "printing": "Printing front sides…",
        }.get(state, f"Front sides: {state}")
        self._status.setText(f"Job {self._job_id}: {text}")
        if state in printer.FINAL_STATES:
            self._timer.stop()

    def _print_back(self):
        try:
            self._submit_back()
        except Exception as e:
            QMessageBox.critical(self, "Print failed", f"Could not print the back sides:\n{e}")
            return
        self.accept()
