#!/usr/bin/env bash
# Build and install Print Studio for the current user, and add the "PrintStudio" printer.
# Run as your normal user; the CUPS parts ask for sudo.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$HOME/.local/bin/printstudio"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

fail() { echo "$*" >&2; exit 1; }

[[ $EUID -ne 0 ]] || fail "Run this as your normal user (it uses sudo where needed)."
command -v cargo >/dev/null || fail "Rust is needed to build Print Studio. On Arch: sudo pacman -S rust"
pkg-config --exists gtk4 libadwaita-1 poppler-glib cups ||
    fail "Missing libraries. On Arch: sudo pacman -S gtk4 libadwaita poppler-glib libcups"

echo "==> Building"
cargo build --release --locked --manifest-path "$ROOT/Cargo.toml" 2>/dev/null ||
    cargo build --release --manifest-path "$ROOT/Cargo.toml"
TARGET="$ROOT/target/release"

echo "==> Installing the printstudio command"
install -Dm755 "$TARGET/printstudio" "$BIN"

echo "==> Adding the app launcher (Open with → Print Studio)"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
install -Dm644 "$ROOT/data/icons/hicolor/scalable/apps/dev.printstudio.PrintStudio.svg" \
    "$ICONS/scalable/apps/dev.printstudio.PrintStudio.svg"
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$ICONS" || true
mkdir -p "$APPS"
rm -f "$APPS/printstudio.desktop"  # name used by early versions
sed "s|@BIN@|$BIN|" "$ROOT/data/dev.printstudio.PrintStudio.desktop" > "$APPS/dev.printstudio.PrintStudio.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" || true

echo "==> Starting the background watcher"
mkdir -p "$UNITS"
sed "s|@BIN@|$BIN|" "$ROOT/systemd/printstudio-watch.service" > "$UNITS/printstudio-watch.service"
systemctl --user daemon-reload
systemctl --user enable printstudio-watch.service
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
sudo install -m 0700 -o root -g root "$TARGET/printstudio-backend" /usr/lib/cups/backend/printstudio
sudo install -d -m 0755 -o root -g root /var/spool/printstudio
sudo lpadmin -p PrintStudio -E -v printstudio:/ -P "$ROOT/cups/PrintStudio.ppd" \
    -D "Print Studio" -L "Opens the Print Studio dialog" \
    -o PageSize="$PAGE" -o printer-is-shared=false

echo
echo "Done. Print to \"PrintStudio\" from any app, or run: printstudio FILE"
echo "(Paper size for the virtual printer: $PAGE)"
