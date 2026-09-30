#!/usr/bin/env bash
# Install Print Studio for the current user and add the "PrintStudio" printer.
# Run as your normal user; the CUPS parts ask for sudo.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
SRC="$ROOT/src"
BIN="$HOME/.local/bin/printstudio"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

if [[ $EUID -eq 0 ]]; then
    echo "Run this as your normal user (it uses sudo where needed)." >&2
    exit 1
fi

python3 -c "import PySide6.QtPdf, pypdf, cups" 2>/dev/null || {
    echo "Missing Python packages. On Arch: sudo pacman -S pyside6 python-pypdf python-pycups python-pillow" >&2
    exit 1
}

echo "==> Installing the printstudio command"
mkdir -p "$(dirname "$BIN")"
cat > "$BIN" <<LAUNCHER
#!/bin/sh
PYTHONPATH="$SRC\${PYTHONPATH:+:\$PYTHONPATH}" exec /usr/bin/python3 -m printstudio "\$@"
LAUNCHER
chmod +x "$BIN"

echo "==> Adding the app launcher (Open with → Print Studio)"
mkdir -p "$APPS"
sed "s|@BIN@|$BIN|" "$ROOT/printstudio.desktop.python" > "$APPS/printstudio.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" || true

echo "==> Starting the background watcher"
mkdir -p "$UNITS"
sed "s|@SRC@|$SRC|" "$ROOT/printstudio-watch.service.python" > "$UNITS/printstudio-watch.service"
systemctl --user daemon-reload
systemctl --user enable --now printstudio-watch.service
systemctl --user restart printstudio-watch.service

# Give the virtual printer the same default paper size as your real printer,
# so apps lay pages out for the paper you actually use.
DEFAULT_PRINTER="$(lpstat -d 2>/dev/null | sed -n 's/^.*destination: //p')"
PAGE=A4
if [[ -n "$DEFAULT_PRINTER" && "$DEFAULT_PRINTER" != PrintStudio ]]; then
    FOUND="$(lpoptions -p "$DEFAULT_PRINTER" -l 2>/dev/null | sed -n 's/^PageSize[^:]*:.*\*\([^ ]*\).*/\1/p')"
    case "$FOUND" in A4|A5|Letter|Legal|Executive) PAGE="$FOUND" ;; esac
fi

echo "==> Adding the PrintStudio printer to CUPS (needs sudo)"
sudo install -m 0700 -o root -g root "$ROOT/cups/printstudio" /usr/lib/cups/backend/printstudio
sudo install -d -m 0755 -o root -g root /var/spool/printstudio
sudo lpadmin -p PrintStudio -E -v printstudio:/ -P "$ROOT/../../cups/PrintStudio.ppd" \
    -D "Print Studio" -L "Opens the Print Studio dialog" \
    -o PageSize="$PAGE" -o printer-is-shared=false

echo
echo "Done. Print to \"PrintStudio\" from any app, or run: printstudio FILE"
echo "(Paper size for the virtual printer: $PAGE)"
