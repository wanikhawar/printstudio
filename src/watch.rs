//! Background service (`printstudio watch`): hands every job the virtual
//! printer captures to Print Studio, which opens it in a window, or asks
//! whether to add it to a job that's already open. Polling once a second is
//! cheap and needs no extra dependencies.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::spool::spool_dir;

const POLL: Duration = Duration::from_secs(1);
const SESSION_VARS: [&str; 6] = ["WAYLAND_DISPLAY", "DISPLAY", "XDG_", "DBUS_SESSION_BUS_ADDRESS", "QT_", "HYPRLAND_"];

/// The graphical session's variables, as systemd currently knows them.
///
/// The service can start before the compositor has exported WAYLAND_DISPLAY
/// and friends, so they are re-read for every window we open.
fn session_env() -> Vec<(String, String)> {
    let Ok(out) = Command::new("systemctl").args(["--user", "show-environment"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(k, v)| SESSION_VARS.iter().any(|p| k.starts_with(p)) && !v.starts_with("$'"))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn launch(pdf: &Path) -> Option<Child> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("printstudio"));
    Command::new(exe)
        .arg("--spool-job")
        .arg(pdf)
        .envs(session_env())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .inspect_err(|e| eprintln!("Couldn't open Print Studio for {}: {e}", pdf.display()))
        .ok()
}

fn jobs(dir: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    name.starts_with("job-") && name.ends_with(".pdf")
                })
                .collect()
        })
        .unwrap_or_default() // the directory appears with the first job
}

pub fn run() -> ! {
    let dir = spool_dir();
    println!("Watching {}", dir.display());
    let mut launched: HashSet<PathBuf> = HashSet::new();
    let mut windows: Vec<Child> = Vec::new();
    loop {
        // Reap closed windows so they don't linger as zombies.
        windows.retain_mut(|w| matches!(w.try_wait(), Ok(None)));
        let current = jobs(&dir);
        let mut new: Vec<&PathBuf> = current.difference(&launched).collect();
        new.sort();
        for pdf in new {
            println!("Opening {}", pdf.file_name().unwrap_or_default().to_string_lossy());
            windows.extend(launch(pdf));
        }
        launched = current;
        std::thread::sleep(POLL);
    }
}
