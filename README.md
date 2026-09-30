# Print Studio

One print dialog for every app. Print to the **PrintStudio** printer from anywhere
(browser, PDF viewer, LibreOffice…) and you get the same window every time, with:

- **Reverse order** (on by default, for printers that stack pages face up)
- Page ranges (`1-3, 7, 10-`), or pick pages by clicking thumbnails; odd/even only
- Copies with **collate** on/off
- 1, 2, 4, 6, 9 or 16 pages per sheet, rotation, scale to fit
- **Guided manual two-sided printing** for printers without a duplexer, plus a calibration test
- Every option your printer's driver offers: quality/speed, colour/grayscale, paper, media type, and the advanced ones
- Presets, and per-printer memory of your last settings
- A live preview of the exact sheets, in print order; double-click any page to zoom in
- Light and dark mode (follows your desktop, or pick one in the ☰ menu)

Written in Rust with GTK 4 and libadwaita. Pages are drawn with poppler; output PDFs are built with lopdf.

## Install

```sh
./install.sh      # builds with cargo, asks for sudo to add the CUPS printer
```

Needs `rust`, `gtk4`, `libadwaita`, `poppler-glib` and `libcups` (all in the Arch repos).

This adds:
- `~/.local/bin/printstudio`, to open a file directly (`printstudio file.pdf`, or drag and drop onto the window)
- a desktop entry, so "Open with → Print Studio" works
- the `printstudio-watch` systemd user service, which opens the window for each captured job
- a CUPS backend + the `PrintStudio` queue

`./uninstall.sh` removes all of it. Your settings stay in `~/.config/printstudio/config.json`.

## Two-sided printing: first-time setup

Printers feed and stack paper in different ways, so run **Print test** once
(under *Manual two-sided printing*). It prints two sheets. Adjust
*Reverse order of back sides* and *Rotate back sides 180°* until both sheets come out
right, then write yourself a note on how the stack goes back in. The settings and
note are saved per printer.

## How it works

```
app ─► CUPS "PrintStudio" queue ─► printstudio-backend ─► /var/spool/printstudio/<user>/job-N.pdf
                                                                  │
                           printstudio watch (user service) ◄─────┘
                                   │
                                   ▼
             Print Studio window ─► page pipeline ─► CUPS ─► real printer
```

| File | What it does |
|---|---|
| `src/pipeline.rs` | All ordering logic: ranges → n-up → odd/even → two-sided → copies → reverse |
| `src/geometry.rs` | Where each page lands on a sheet, shared by the preview and the PDF writer |
| `src/render.rs` | Draws pages and output sheets on screen (poppler + cairo), without writing PDFs |
| `src/pdfout.rs` | Builds the print-ready PDFs (lopdf) when you print |
| `src/cups.rs`, `src/ppd.rs` | Printers, their options and paper sizes; submitting jobs |
| `src/ui/` | The window, thumbnail grids and page viewer |
| `src/bin/printstudio-backend.rs` | The CUPS backend (runs as root, drops to the printing user) |

## Development

```sh
cargo test                  # includes a check that the preview matches the printed PDF pixel for pixel
cargo run -- file.pdf
```

`cargo build --features devshot` adds a scripting hook (`PRINTSTUDIO_DEVSCRIPT`) for
driving the window and taking screenshots headlessly, e.g. under `gtk4-broadwayd`.

## Troubleshooting

- Nothing opens after printing to PrintStudio: `systemctl --user status printstudio-watch`
  and `journalctl --user -u printstudio-watch`
- The job is stuck in CUPS: `lpstat -o PrintStudio`, and check `/var/log/cups/error_log`
- Big documents from some apps (Okular sends huge PostScript) take a while to reach
  Print Studio: CUPS converts them to PDF first
