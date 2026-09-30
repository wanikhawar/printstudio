"""Background service: opens Print Studio for every job the virtual printer captures.

Run as ``python -m printstudio.watch`` (the systemd user unit does this). It
polls the spool directory, which is cheap and needs no extra dependencies.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

from .spool import spool_dir

POLL_SECONDS = 1.0
SESSION_VARS = ("WAYLAND_DISPLAY", "DISPLAY", "XDG_", "DBUS_SESSION_BUS_ADDRESS", "QT_", "HYPRLAND_")


def session_env() -> dict[str, str]:
    """os.environ plus the graphical session's variables.

    The service can start before the compositor has exported WAYLAND_DISPLAY
    and friends to systemd, so re-read them for every window we open.
    """
    env = os.environ.copy()
    try:
        out = subprocess.run(["systemctl", "--user", "show-environment"],
                             capture_output=True, text=True, timeout=5).stdout
    except (OSError, subprocess.TimeoutExpired):
        return env
    for line in out.splitlines():
        key, sep, value = line.partition("=")
        if sep and key.startswith(SESSION_VARS) and not value.startswith("$'"):
            env[key] = value
    return env


def launch(pdf: Path) -> None:
    subprocess.Popen(
        [sys.executable, "-m", "printstudio", "--spool-job", str(pdf)],
        env=session_env(), start_new_session=True,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
    )


def main() -> int:
    directory = spool_dir()
    print(f"Watching {directory}", flush=True)
    launched: set[Path] = set()
    while True:
        try:
            current = set(directory.glob("job-*.pdf"))
        except OSError:
            current = set()  # directory appears with the first job
        for pdf in sorted(current - launched):
            print(f"Opening {pdf.name}", flush=True)
            launch(pdf)
        launched = (launched | current) & current
        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    sys.exit(main())
