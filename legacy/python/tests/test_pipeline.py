import io
import unittest

from pypdf import PdfReader

from printstudio.pdfgen import numbered_pdf
from printstudio.pipeline import (
    JobOptions, describe_side, format_ranges, parse_ranges, plan, render_pass,
)


def opts(**kw) -> JobOptions:
    kw.setdefault("reverse", False)
    return JobOptions(**kw)


def order(passes, i=0):
    return [tuple(p + 1 for p in side) for side in passes[i].sides]


def reader(n: int) -> PdfReader:
    buf = io.BytesIO()
    numbered_pdf(n).write(buf)
    return PdfReader(io.BytesIO(buf.getvalue()))


def texts(writer) -> list[str]:
    buf = io.BytesIO()
    writer.write(buf)
    return [p.extract_text().strip() for p in PdfReader(io.BytesIO(buf.getvalue())).pages]


class ParseRanges(unittest.TestCase):
    def test_all(self):
        self.assertEqual(parse_ranges("", 3), [0, 1, 2])

    def test_mixed(self):
        self.assertEqual(parse_ranges("1-2, 5, 8-", 9), [0, 1, 4, 7, 8])

    def test_open_start_and_clip(self):
        self.assertEqual(parse_ranges("-2,4-99", 5), [0, 1, 3, 4])

    def test_format_round_trip(self):
        self.assertEqual(format_ranges([0, 1, 2, 6, 8, 9]), "1-3, 7, 9-10")
        self.assertEqual(format_ranges([]), "")
        self.assertEqual(parse_ranges(format_ranges([4, 0, 1]), 9), [0, 1, 4])

    def test_invalid(self):
        for bad in ("x", "3-1", "0", "1-a"):
            with self.assertRaises(ValueError):
                parse_ranges(bad, 5)


class Plan(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(order(plan(3, opts())), [(1,), (2,), (3,)])

    def test_reverse(self):
        self.assertEqual(order(plan(3, opts(reverse=True))), [(3,), (2,), (1,)])

    def test_odd_even(self):
        self.assertEqual(order(plan(5, opts(page_set="odd"))), [(1,), (3,), (5,)])
        self.assertEqual(order(plan(5, opts(page_set="even"))), [(2,), (4,)])

    def test_collate(self):
        self.assertEqual(order(plan(2, opts(copies=2))), [(1,), (2,), (1,), (2,)])
        self.assertEqual(order(plan(2, opts(copies=2, collate=False))), [(1,), (1,), (2,), (2,)])

    def test_reverse_collated_copies(self):
        self.assertEqual(order(plan(2, opts(copies=2, reverse=True))), [(2,), (1,), (2,), (1,)])

    def test_nup(self):
        self.assertEqual(order(plan(5, opts(nup=2))), [(1, 2), (3, 4), (5,)])

    def test_nothing_selected(self):
        with self.assertRaises(ValueError):
            plan(3, opts(page_ranges="7-9"))

    def test_duplex_pads_odd_count(self):
        passes = plan(5, opts(duplex=True))
        self.assertEqual(order(passes, 0), [(1,), (3,), (5,)])
        self.assertEqual(order(passes, 1), [(2,), (4,), ()])

    def test_duplex_reverse_fronts_only(self):
        passes = plan(4, opts(duplex=True, reverse=True))
        self.assertEqual(order(passes, 0), [(3,), (1,)])
        self.assertEqual(order(passes, 1), [(2,), (4,)])

    def test_duplex_back_settings(self):
        passes = plan(4, opts(duplex=True, backs_reverse=True, backs_rotate=True))
        self.assertEqual(order(passes, 1), [(4,), (2,)])
        self.assertTrue(passes[1].rotate180)

    def test_duplex_copies_start_on_new_sheet(self):
        passes = plan(3, opts(duplex=True, copies=2))
        self.assertEqual(order(passes, 0), [(1,), (3,), (1,), (3,)])
        self.assertEqual(order(passes, 1), [(2,), (), (2,), ()])

    def test_duplex_uncollated_repeats_sheets(self):
        passes = plan(4, opts(duplex=True, copies=2, collate=False))
        self.assertEqual(order(passes, 0), [(1,), (1,), (3,), (3,)])
        self.assertEqual(order(passes, 1), [(2,), (2,), (4,), (4,)])

    def test_describe(self):
        self.assertEqual(describe_side((0, 1, 2, 3)), "pp. 1–4")
        self.assertEqual(describe_side(()), "blank")


class Render(unittest.TestCase):
    def test_reverse_output(self):
        r = reader(3)
        o = opts(reverse=True)
        self.assertEqual(texts(render_pass(r, plan(3, o)[0], o)), ["P3", "P2", "P1"])

    def test_copies_are_separate_pages(self):
        r = reader(2)
        o = opts(copies=3)
        self.assertEqual(texts(render_pass(r, plan(2, o)[0], o)), ["P1", "P2"] * 3)

    def test_duplex_blank_and_rotation(self):
        r = reader(3)
        o = opts(duplex=True, backs_rotate=True)
        backs = render_pass(r, plan(3, o)[1], o)
        self.assertEqual(texts(backs), ["P2", ""])
        self.assertEqual([p.rotation for p in backs.pages], [180, 180])

    def test_nup_portrait_grid(self):
        r = reader(5)
        o = opts(nup=4)
        w = render_pass(r, plan(5, o)[0], o, paper=(595, 842))
        self.assertEqual(len(w.pages), 2)
        self.assertEqual(texts(w)[1], "P5")
        for token in ("P1", "P2", "P3", "P4"):
            self.assertIn(token, texts(w)[0])
        self.assertEqual((float(w.pages[0].mediabox.width), float(w.pages[0].mediabox.height)), (595, 842))

    def test_nup_landscape_stays_on_portrait_paper(self):
        r = reader(2)
        o = opts(nup=2)
        w = render_pass(r, plan(2, o)[0], o, paper=(612, 792))
        self.assertEqual(len(w.pages), 1)
        self.assertEqual((float(w.pages[0].mediabox.width), float(w.pages[0].mediabox.height)), (612, 792))
        self.assertIn("P1", texts(w)[0])
        self.assertIn("P2", texts(w)[0])


if __name__ == "__main__":
    unittest.main()
