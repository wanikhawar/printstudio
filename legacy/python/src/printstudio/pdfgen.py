"""Build simple text-only PDFs (calibration sheets, test documents) with pypdf."""

from __future__ import annotations

from pypdf import PdfWriter
from pypdf.generic import DecodedStreamObject, DictionaryObject, NameObject

A4 = (595.0, 842.0)


def _escape(text: str) -> bytes:
    text = text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
    return text.encode("latin-1", "replace")


def add_text_page(writer: PdfWriter, lines: list[tuple[int, str]],
                  size: tuple[float, float] = A4) -> None:
    """Add a page with (font_size, text) lines, flowing down from the top."""
    w, h = size
    page = writer.add_blank_page(w, h)
    font = DictionaryObject({
        NameObject("/Type"): NameObject("/Font"),
        NameObject("/Subtype"): NameObject("/Type1"),
        NameObject("/BaseFont"): NameObject("/Helvetica"),
        NameObject("/Encoding"): NameObject("/WinAnsiEncoding"),
    })
    page[NameObject("/Resources")] = DictionaryObject({
        NameObject("/Font"): DictionaryObject({NameObject("/F1"): font}),
    })
    ops = []
    y = h - 60
    for size_pt, text in lines:
        y -= size_pt * 1.4
        ops.append(b"BT /F1 %d Tf 50 %.1f Td (%s) Tj ET" % (size_pt, y, _escape(text)))
    stream = DecodedStreamObject()
    stream.set_data(b"\n".join(ops))
    page[NameObject("/Contents")] = writer._add_object(stream)


def numbered_pdf(n: int, size: tuple[float, float] = A4) -> PdfWriter:
    writer = PdfWriter()
    for i in range(1, n + 1):
        add_text_page(writer, [(36, f"P{i}")], size)
    return writer


def duplex_test_pdf(size: tuple[float, float] = A4) -> PdfWriter:
    """Four sides (two sheets) that show whether back sides line up."""
    writer = PdfWriter()
    for sheet in (1, 2):
        add_text_page(writer, [
            (14, "^^^ TOP OF PAGE ^^^"),
            (40, f"SHEET {sheet}"),
            (40, "FRONT"),
            (12, ""),
            (12, "Two-sided calibration test from Print Studio."),
            (12, f"Turn this sheet over: the back must say SHEET {sheet} - BACK,"),
            (12, "and its TOP OF PAGE must be on the same edge as this one."),
        ], size)
        add_text_page(writer, [
            (14, "^^^ TOP OF PAGE ^^^"),
            (40, f"SHEET {sheet}"),
            (40, "BACK"),
            (12, ""),
            (12, f"If this side is not behind SHEET {sheet} - FRONT, turn on or off:"),
            (12, "    'Reverse order of back sides'"),
            (12, "If this side is upside down compared to the front, turn on or off:"),
            (12, "    'Rotate back sides 180 degrees'"),
            (12, "Then print the test again until both sheets are right."),
        ], size)
    return writer
