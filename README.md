# Print Studio

One print dialog for every app. Print to the **PrintStudio** printer from anywhere
(browser, PDF viewer, LibreOffice…) and you get the same window every time, with:

- **Reverse order** (on by default, for printers that stack pages face up)
- Page ranges (`1-3, 7, 10-`), or pick pages by clicking thumbnails; odd/even only
- Copies with **collate** on/off
- 1, 2, 4, 6, 9 or 16 pages per sheet, rotation, scale to fit
- **Booklets**: fold the printed stack in half and it reads like a book (A4 → A5).
  Split thick ones into several thinner booklets, bind on the right for
  right-to-left text, and add a gutter at the fold
- **Several documents in one job**: print a second document while one is open and
  Print Studio asks whether to add it to the job or open it separately. Reorder
  documents by dragging, and optionally start each one on a fresh sheet
- **Guided manual two-sided printing** for printers without a duplexer, plus a calibration test
- Every option your printer's driver offers: quality/speed, colour/grayscale, paper, media type, and the advanced ones
- Presets, and per-printer memory of your last settings
- A live preview of the exact sheets, in print order; double-click any page to zoom in
- A running count of the paper you'll use, and how much n-up, two-sided and booklets save
- Light and dark mode (follows your desktop, or pick one in the ☰ menu); the settings
  sidebar folds away on narrow windows (F9 shows or hides it)

Written in Rust with GTK 4 and libadwaita. Pages are drawn with poppler; output PDFs are built with lopdf.

## Install

```sh
./install.sh      # builds with cargo, asks for sudo to add the CUPS printer
```

Needs `rust`, `gtk4`, `libadwaita`, `poppler-glib` and `libcups` (all in the Arch repos).

This adds:
- `~/.local/bin/printstudio`, to open a file directly (`printstudio file.pdf`, or drag and drop onto the window)
- a desktop entry and app icon, so "Open with → Print Studio" works
- the `printstudio-watch` systemd user service, which opens the window for each captured job
- a CUPS backend + the `PrintStudio` queue

`./uninstall.sh` removes all of it. Your settings stay in `~/.config/printstudio/config.json`.

## Two-sided printing: first-time setup

Printers feed and stack paper in different ways, so run **Print test** once
(under *Manual two-sided printing*). It prints two sheets. Adjust
*Reverse order of back sides* and *Rotate back sides 180°* until both sheets come out
right, then write yourself a note on how the stack goes back in. The settings and
note are saved per printer.

Booklets use the same settings: their sheets flip on the short edge, and Print
Studio turns the back sides for that itself, so there's nothing extra to calibrate.

## Booklets

Turn on **Booklet** under *Layout*. Two pages go side by side on each side of the
sheet, in folding order, and the job always prints two-sided (guided, like manual
two-sided printing). When the back sides are done, keep the stack in order, fold it
in half, and page 1 is on the cover. For long documents, set *Sheets per booklet*
(say 4): the stack then makes several thin booklets that fold flat and stack into
one book.

## How it works

```
app ─► CUPS "PrintStudio" queue ─► printstudio-backend ─► /var/spool/printstudio/<user>/job-N.pdf
                                                                  │
                           printstudio watch (user service) ◄─────┘
                                   │
                                   ▼
        Print Studio (one copy running; new jobs go to the open window)
                                   │
                                   ▼
                    page pipeline ─► CUPS ─► real printer
```

| File | What it does |
|---|---|
| `src/pipeline.rs` | All ordering logic: ranges → n-up or booklet → odd/even → two-sided → copies → reverse |
| `src/geometry.rs` | Where each page lands on a sheet, shared by the preview and the PDF writer |
| `src/render.rs` | Draws pages and output sheets on screen (poppler + cairo), without writing PDFs |
| `src/pdfout.rs` | Builds the print-ready PDFs (lopdf) when you print, and joins several documents into one |
| `src/cups.rs`, `src/ppd.rs` | Printers, their options and paper sizes; submitting jobs |
| `src/ui/` | The window, thumbnail grids, page viewer and instruction drawings |
| `src/bin/printstudio-backend.rs` | The CUPS backend (runs as root, drops to the printing user) |

## Development

```sh
cargo test                  # includes a check that the preview matches the printed PDF pixel for pixel
cargo run -- file.pdf
```

`cargo build --features devshot` adds a scripting hook (`PRINTSTUDIO_DEVSCRIPT`) for
driving the window and taking screenshots headlessly, e.g. under `gtk4-broadwayd` or
`Xvfb`. For example `booklet=1;add=/path/other.pdf;wait=1500;shot=/tmp/a.png;quit`.

## Troubleshooting

- Nothing opens after printing to PrintStudio: `systemctl --user status printstudio-watch`
  and `journalctl --user -u printstudio-watch`
- The job is stuck in CUPS: `lpstat -o PrintStudio`, and check `/var/log/cups/error_log`
- Big documents from some apps (Okular sends huge PostScript) take a while to reach
  Print Studio: CUPS converts them to PDF first
