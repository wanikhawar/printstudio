"""Entry point: ``printstudio [FILE]`` or ``printstudio --spool-job JOB.pdf``."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="printstudio", description="One print dialog for every app.")
    parser.add_argument("file", nargs="?", type=Path, help="document to print")
    parser.add_argument("--spool-job", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)

    from PySide6.QtWidgets import QApplication, QMessageBox

    from . import printer
    from .config import Config
    from .spool import SpoolJob
    from .ui.main_window import MainWindow

    app = QApplication(sys.argv[:1])
    app.setApplicationName("Print Studio")
    app.setDesktopFileName("printstudio")

    try:
        conn = printer.connect()
    except RuntimeError as e:
        QMessageBox.critical(None, "Print Studio", f"Can't reach the printing service (CUPS): {e}")
        return 1

    spool_job = SpoolJob.load(args.spool_job) if args.spool_job else None
    window = MainWindow(Config(), conn, path=args.file, spool_job=spool_job)
    window.show()
    window.raise_()
    window.activateWindow()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
