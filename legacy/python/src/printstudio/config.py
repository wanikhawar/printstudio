"""Persistent settings: per-printer defaults, presets and the last printer used.

Stored as JSON in $XDG_CONFIG_HOME/printstudio/config.json. A "settings"
entry is ``{"job": JobOptions dict, "ppd": {keyword: value}}``.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

from .pipeline import JobOptions

# Never carried over from one document to the next.
PER_DOCUMENT = {"page_ranges", "copies"}
# Describe how the printer handles paper, so presets leave them alone.
PER_PRINTER = {"backs_reverse", "backs_rotate"}


def config_path() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config"
    return Path(base) / "printstudio" / "config.json"


class Config:
    def __init__(self, path: Path | None = None):
        self.path = path or config_path()
        try:
            self.data = json.loads(self.path.read_text())
        except (OSError, ValueError):
            self.data = {}
        self.data.setdefault("printers", {})
        self.data.setdefault("presets", {})
        self.data.setdefault("ui", {})

    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.data, indent=2, sort_keys=True))
        tmp.replace(self.path)

    @property
    def ui(self) -> dict:
        """Window preferences such as thumbnail size."""
        return self.data["ui"]

    @property
    def last_printer(self) -> str | None:
        return self.data.get("last_printer")

    @last_printer.setter
    def last_printer(self, name: str) -> None:
        self.data["last_printer"] = name

    # Per-printer defaults

    def printer_settings(self, printer: str) -> tuple[JobOptions, dict[str, str]]:
        entry = self.data["printers"].get(printer, {})
        return JobOptions.from_dict(entry.get("job", {})), dict(entry.get("ppd", {}))

    def set_printer_settings(self, printer: str, job: JobOptions, ppd: dict[str, str]) -> None:
        job_dict = {k: v for k, v in job.to_dict().items() if k not in PER_DOCUMENT}
        entry = self.data["printers"].setdefault(printer, {})
        entry.update(job=job_dict, ppd=dict(ppd))

    def printer_note(self, printer: str) -> str:
        return self.data["printers"].get(printer, {}).get("duplex_note", "")

    def set_printer_note(self, printer: str, note: str) -> None:
        self.data["printers"].setdefault(printer, {})["duplex_note"] = note

    # Presets

    def preset_names(self) -> list[str]:
        return sorted(self.data["presets"], key=str.casefold)

    def preset(self, name: str) -> tuple[dict, dict[str, str]]:
        """Returns the stored job fields (partial) and PPD options."""
        entry = self.data["presets"].get(name, {})
        return dict(entry.get("job", {})), dict(entry.get("ppd", {}))

    def set_preset(self, name: str, job: JobOptions, ppd: dict[str, str]) -> None:
        job_dict = {k: v for k, v in job.to_dict().items()
                    if k not in PER_DOCUMENT | PER_PRINTER}
        self.data["presets"][name] = {"job": job_dict, "ppd": dict(ppd)}

    def delete_preset(self, name: str) -> None:
        self.data["presets"].pop(name, None)
