"""Thin pycups wrapper: printers, their PPD options, job submission and status."""

from __future__ import annotations

import os
import re
from dataclasses import dataclass, field

import cups

VIRTUAL_URI_SCHEME = "printstudio:"

# Options that get a spot in the main "Printer settings" section; the rest go
# under "Advanced". Matched against the PPD keyword and its label.
COMMON_OPTION = re.compile(
    r"^(PageSize|MediaType|InputSlot|ColorModel|Resolution|cupsPrintQuality|OutputMode)$"
    r"|quality|resolution|colou?r ?model|colou?r ?/ ?gr[ae]yscale|gr[ae]yscale|mono ?colou?r|media ?type",
    re.IGNORECASE,
)
HIDDEN_OPTIONS = {"PageRegion"}  # PPD duplicate of PageSize

# Fallback sizes (points) when a printer has no PPD to read them from.
PAPER_SIZES = {
    "A4": (595.0, 842.0), "A5": (420.0, 595.0), "A6": (297.0, 420.0),
    "Letter": (612.0, 792.0), "Legal": (612.0, 1008.0), "Executive": (522.0, 756.0),
}

JOB_STATES = {
    3: "queued", 4: "held", 5: "printing", 6: "stopped",
    7: "cancelled", 8: "aborted", 9: "done",
}
FINAL_STATES = {"cancelled", "aborted", "done"}


@dataclass
class PrinterOption:
    keyword: str
    label: str
    group: str
    default: str
    choices: list[tuple[str, str]]  # (value, label)
    common: bool = False


@dataclass
class PrinterInfo:
    name: str
    options: list[PrinterOption] = field(default_factory=list)
    paper_sizes: dict[str, tuple[float, float]] = field(default_factory=dict)

    def paper(self, page_size: str | None) -> tuple[float, float] | None:
        if page_size is None:
            return None
        return self.paper_sizes.get(page_size) or PAPER_SIZES.get(page_size)


def connect() -> cups.Connection:
    return cups.Connection()


def list_printers(conn: cups.Connection) -> list[str]:
    """Real printers only: the Print Studio virtual queue is left out."""
    return sorted(
        name for name, attrs in conn.getPrinters().items()
        if not attrs.get("device-uri", "").startswith(VIRTUAL_URI_SCHEME)
    )


def default_printer(conn: cups.Connection, printers: list[str]) -> str | None:
    candidates = [conn.getDefault()]
    try:
        # The user's own default from lpoptions shows up as the (None, None) dest.
        dest = conn.getDests().get((None, None))
        candidates.append(dest.name if dest else None)
    except cups.IPPError:
        pass
    for name in candidates:
        if name in printers:
            return name
    return printers[0] if printers else None


def _paper_dimensions(ppd_text: str) -> dict[str, tuple[float, float]]:
    sizes = {}
    for name, dims in re.findall(r'^\*PaperDimension\s+([^/:\s]+)[^:]*:\s*"([^"]+)"', ppd_text, re.M):
        w, h = dims.split()[:2]
        sizes[name] = (float(w), float(h))
    return sizes


def printer_info(conn: cups.Connection, name: str) -> PrinterInfo:
    info = PrinterInfo(name)
    try:
        path = conn.getPPD(name)
    except cups.IPPError:
        return info  # driverless queue without a PPD: fall back to plain CUPS options
    try:
        with open(path, encoding="latin-1") as f:
            info.paper_sizes = _paper_dimensions(f.read())
        ppd = cups.PPD(path)
    finally:
        os.unlink(path)

    try:
        saved = conn.getDests().get((name, None))
        saved = saved.options if saved else {}
    except cups.IPPError:
        saved = {}

    def walk(groups):
        for group in groups:
            for opt in group.options:
                if opt.keyword in HIDDEN_OPTIONS or not opt.choices:
                    continue
                choices = [(c["choice"], c["text"]) for c in opt.choices]
                values = {v for v, _ in choices}
                default = saved.get(opt.keyword)
                if default not in values:
                    default = opt.defchoice if opt.defchoice in values else choices[0][0]
                info.options.append(PrinterOption(
                    opt.keyword, opt.text or opt.keyword, group.text or group.name,
                    default, choices,
                    bool(COMMON_OPTION.search(opt.keyword) or COMMON_OPTION.search(opt.text or "")),
                ))
            walk(group.subgroups)

    walk(ppd.optionGroups)
    return info


def submit(conn: cups.Connection, printer: str, path: str, title: str, options: dict[str, str]) -> int:
    return conn.printFile(printer, path, title, {k: str(v) for k, v in options.items()})


def job_state(conn: cups.Connection, job_id: int) -> str:
    try:
        attrs = conn.getJobAttributes(job_id, requested_attributes=["job-state"])
    except cups.IPPError:
        return "unknown"
    return JOB_STATES.get(attrs.get("job-state"), "unknown")
