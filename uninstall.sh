#!/usr/bin/env bash
# Remove everything install.sh added. Your settings in ~/.config/printstudio are kept.
set -uo pipefail

APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
systemctl --user disable --now printstudio-watch.service 2>/dev/null
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/printstudio-watch.service"
systemctl --user daemon-reload
rm -f "$HOME/.local/bin/printstudio" "$APPS/dev.printstudio.PrintStudio.desktop" "$APPS/printstudio.desktop"
rm -f "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps/dev.printstudio.PrintStudio.svg"

sudo lpadmin -x PrintStudio 2>/dev/null
sudo rm -f /usr/lib/cups/backend/printstudio
sudo rm -rf /var/spool/printstudio
echo "Print Studio removed."
