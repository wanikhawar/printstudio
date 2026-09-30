# Print Studio

One print dialog for every app. Print to the **PrintStudio** printer from anywhere
(browser, PDF viewer, LibreOffice…) and you get the same window every time, with:

- **Reverse order** (on by default, for printers that stack pages face up)
- Page ranges (`1-3, 7, 10-`), odd/even only
- Copies with **collate** on/off
- 1, 2, 4, 6, 9 or 16 pages per sheet, rotation, scale to fit
- **Guided manual two-sided printing** for printers without a duplexer, plus a calibration test
- Every option your printer's driver offers: quality/speed, colour/grayscale, paper, media type, and the advanced ones
- Presets, and per-printer memory of your last settings
- A live preview of the exact sheets, in the exact order they will print

## Install

```sh
./install.sh      # asks for sudo to add the CUPS printer
```

This adds:
- `~/.local/bin/printstudio`, to open a file directly (`printstudio file.pdf`, or drag and drop onto the window)
- a desktop entry, so "Open with → Print Studio" works
- the `printstudio-watch` systemd user service, which opens the window for each captured job
- a CUPS backend + the `PrintStudio` queue

`./uninstall.sh` removes all of it. Your settings stay in `~/.config/printstudio/config.json`.

## Two-sided printing: first-time setup

Printers feed and stack paper in different ways, so run **Print calibration test** once
(in the *Manual two-sided printing* box). It prints two sheets. Adjust
*Reverse order of back sides* and *Rotate back sides 180°* until both sheets come out
right, then write yourself a note on how the stack goes back in. The settings and
note are saved per printer.

## How it works

```
app ─► CUPS "PrintStudio" queue ─► cups/printstudio backend ─► /var/spool/printstudio/<user>/job-N.pdf
                                                                   │
                         printstudio-watch (user service) ◄────────┘
                                   │
                                   ▼
             Print Studio window ─► page pipeline (pypdf) ─► CUPS ─► real printer
```

- `src/printstudio/pipeline.py`: all ordering logic (ranges → n-up → odd/even → duplex → copies → reverse)
- `src/printstudio/printer.py`: reads printers and their options from CUPS/PPD, submits jobs
- `src/printstudio/ui/`: the window and the two-sided dialog

## Tests

```sh
PYTHONPATH=src python3 -m unittest discover -s tests
```

## Troubleshooting

- Nothing opens after printing to PrintStudio: `systemctl --user status printstudio-watch`
  and `journalctl --user -u printstudio-watch`
- The job is stuck in CUPS: `lpstat -o PrintStudio`, and check `/var/log/cups/error_log`
