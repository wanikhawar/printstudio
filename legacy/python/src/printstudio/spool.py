"""Jobs captured by the "PrintStudio" CUPS queue.

The CUPS backend (cups/printstudio) writes ``job-<id>.json`` and then
``job-<id>.pdf`` into the user's spool directory. The PDF is written last, so
once it exists the job is complete.
"""

from __future__ import annotations

import getpass
import json
import os
from dataclasses import dataclass
from pathlib import Path

SPOOL_ROOT = Path("/var/spool/printstudio")


def spool_dir() -> Path:
    override = os.environ.get("PRINTSTUDIO_SPOOL")
    return Path(override) if override else SPOOL_ROOT / getpass.getuser()


@dataclass
class SpoolJob:
    pdf: Path
    title: str = ""
    copies: int = 1

    @classmethod
    def load(cls, pdf: Path) -> "SpoolJob":
        try:
            meta = json.loads(pdf.with_suffix(".json").read_text())
        except (OSError, ValueError):
            meta = {}
        try:
            copies = max(1, int(meta.get("copies", 1)))
        except (TypeError, ValueError):
            copies = 1
        return cls(pdf, str(meta.get("title", "")), copies)

    def remove(self) -> None:
        for path in (self.pdf, self.pdf.with_suffix(".json")):
            path.unlink(missing_ok=True)
