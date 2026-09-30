"""Page pipeline: turn a source PDF plus JobOptions into print-ready PDFs.

The work is split in two:

* ``plan()`` is pure index arithmetic. It decides which source pages land on
  which side of which sheet, in what order and in how many passes. It has no
  PDF dependency, so every ordering rule is easy to test.
* ``render_pass()`` turns one planned pass into a PDF with pypdf.

Order of operations: page ranges -> n-up (pages onto sides) -> odd/even
(counted on output sides, like CUPS ``page-set``) -> duplex padding ->
copies/collate -> reverse.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, fields

from pypdf import PageObject, PdfReader, PdfWriter, Transformation

# Source page indices printed on one side of a sheet. Empty means a blank side.
Side = tuple[int, ...]

# Pages per side -> (columns, rows, landscape). Landscape layouts are composed
# on a landscape canvas and then turned onto the portrait paper, which is what
# CUPS number-up does too.
NUP_LAYOUTS: dict[int, tuple[int, int, bool]] = {
    1: (1, 1, False),
    2: (2, 1, True),
    4: (2, 2, False),
    6: (3, 2, True),
    9: (3, 3, False),
    16: (4, 4, False),
}


@dataclass
class JobOptions:
    page_ranges: str = ""        # "1-3,7,10-"; empty means all pages
    page_set: str = "all"        # all | odd | even, counted on output sides
    copies: int = 1
    collate: bool = True
    reverse: bool = True         # face-up output trays need the last page printed first
    nup: int = 1
    rotate: int = 0              # 0 / 90 / 180 / 270, applied to every output side
    fit_to_page: bool = False
    duplex: bool = False         # guided manual two-sided printing
    backs_reverse: bool = False  # print pass 2 in the opposite order
    backs_rotate: bool = False   # turn pass 2 sides by 180 degrees

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> "JobOptions":
        known = {f.name for f in fields(cls)}
        return cls(**{k: v for k, v in data.items() if k in known})


@dataclass
class Pass:
    """One trip of paper through the printer."""

    sides: list[Side]
    rotate180: bool = False
    label: str = ""


def parse_ranges(spec: str, n_pages: int) -> list[int]:
    """Parse "1-3, 7, 10-" into 0-based page indices. Pages past the end are dropped."""
    if not spec.strip():
        return list(range(n_pages))
    pages: list[int] = []
    for part in spec.replace(" ", "").split(","):
        if not part:
            continue
        try:
            if "-" in part:
                a, b = part.split("-", 1)
                start = int(a) if a else 1
                end = int(b) if b else n_pages
            else:
                start = end = int(part)
        except ValueError:
            raise ValueError(f"Invalid page range: {part!r}") from None
        if start < 1 or end < start:
            raise ValueError(f"Invalid page range: {part!r}")
        pages.extend(range(start - 1, min(end, n_pages)))
    return pages


def format_ranges(pages: list[int]) -> str:
    """0-based indices -> "1-3, 7". The inverse of parse_ranges for sorted input."""
    parts = []
    run_start = prev = None
    for p in sorted(set(pages)) + [None]:
        if prev is not None and p == prev + 1:
            prev = p
            continue
        if run_start is not None:
            parts.append(str(run_start + 1) if run_start == prev else f"{run_start + 1}-{prev + 1}")
        run_start = prev = p
    return ", ".join(parts)


def plan(n_pages: int, opts: JobOptions) -> list[Pass]:
    if opts.nup not in NUP_LAYOUTS:
        raise ValueError(f"Unsupported pages per sheet: {opts.nup}")
    pages = parse_ranges(opts.page_ranges, n_pages)
    sides: list[Side] = [tuple(pages[i:i + opts.nup]) for i in range(0, len(pages), opts.nup)]
    if opts.page_set == "odd":
        sides = sides[0::2]
    elif opts.page_set == "even":
        sides = sides[1::2]
    if not sides:
        raise ValueError("No pages selected")

    # A unit is what must stay together when making copies: one side, or a
    # front/back pair when printing two-sided.
    per_unit = 2 if opts.duplex else 1
    if opts.duplex and len(sides) % 2:
        sides.append(())
    units = [sides[i:i + per_unit] for i in range(0, len(sides), per_unit)]
    copies = max(1, opts.copies)
    if opts.collate:
        units = units * copies
    else:
        units = [u for u in units for _ in range(copies)]

    if not opts.duplex:
        seq = [s for u in units for s in u]
        if opts.reverse:
            seq.reverse()
        return [Pass(seq, label="Pages")]

    fronts = [u[0] for u in units]
    backs = [u[1] for u in units]
    if opts.reverse:
        fronts.reverse()
    if opts.backs_reverse:
        backs.reverse()
    return [
        Pass(fronts, label="Front"),
        Pass(backs, rotate180=opts.backs_rotate, label="Back"),
    ]


def describe_side(side: Side) -> str:
    if not side:
        return "blank"
    nums = [i + 1 for i in side]
    if len(nums) == 1:
        return f"p. {nums[0]}"
    if nums == list(range(nums[0], nums[-1] + 1)):
        return f"pp. {nums[0]}–{nums[-1]}"
    return "pp. " + ",".join(map(str, nums))


def _box(page: PageObject) -> tuple[float, float, float, float]:
    box = page.mediabox
    return float(box.left), float(box.bottom), float(box.width), float(box.height)


def _page_size(page: PageObject) -> tuple[float, float]:
    _, _, w, h = _box(page)
    if page.rotation % 180:
        w, h = h, w
    return w, h


def _compose(writer: PdfWriter, reader: PdfReader, side: Side, nup: int,
             paper: tuple[float, float]) -> PageObject:
    cols, rows, landscape = NUP_LAYOUTS[nup]
    pw, ph = paper
    cw, ch = (ph, pw) if landscape else (pw, ph)  # canvas size
    cell_w, cell_h = cw / cols, ch / rows
    sheet = writer.add_blank_page(pw, ph)
    for slot, idx in enumerate(side):
        src = reader.pages[idx]
        if src.rotation:
            src.transfer_rotation_to_content()
        left, bottom, w, h = _box(src)
        scale = min(cell_w / w, cell_h / h)
        col, row = slot % cols, slot // cols
        x = col * cell_w + (cell_w - w * scale) / 2
        y = ch - (row + 1) * cell_h + (cell_h - h * scale) / 2
        t = Transformation().translate(-left, -bottom).scale(scale).translate(x, y)
        if landscape:
            # (x, y) on the landscape canvas -> (pw - y, x) on the portrait sheet
            t = t.rotate(90).translate(pw, 0)
        sheet.merge_transformed_page(src, t)
    return sheet


def render_pass(reader: PdfReader, p: Pass, opts: JobOptions,
                paper: tuple[float, float] | None = None,
                writer: PdfWriter | None = None) -> PdfWriter:
    """Render one pass. Pass ``writer`` to append to an existing document."""
    writer = writer or PdfWriter()
    first = next((i for side in p.sides for i in side), 0)
    blank_size = paper or _page_size(reader.pages[first])
    if opts.nup > 1 and paper is None:
        paper = _page_size(reader.pages[first])
    rotation = (opts.rotate + (180 if p.rotate180 else 0)) % 360
    for side in p.sides:
        if not side:
            out = writer.add_blank_page(*blank_size)
        elif opts.nup == 1:
            out = writer.add_page(reader.pages[side[0]])
        else:
            out = _compose(writer, reader, side, opts.nup, paper)
        if rotation:
            out.rotate(rotation)
    return writer
