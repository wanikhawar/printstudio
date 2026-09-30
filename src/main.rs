//! printstudio [FILE]            open a document
//! printstudio --spool-job JOB    open a job captured by the PrintStudio printer
//! printstudio watch              background service that does the above

mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use printstudio::watch;

const USAGE: &str = "Usage: printstudio [FILE]\n       printstudio watch\n\nOne print dialog for every app.";

/// An absolute path, so a copy of Print Studio that's already running (in
/// another directory) opens the right file.
fn absolute(path: &str) -> String {
    let p = PathBuf::from(path);
    let p = if p.is_absolute() { p } else { std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p) };
    p.to_string_lossy().into_owned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let forwarded = match args.as_slice() {
        [] => Vec::new(),
        [cmd] if cmd == "watch" => watch::run(),
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        [flag, job] if flag == "--spool-job" => vec![flag.clone(), absolute(job)],
        [file] if !file.starts_with('-') => vec![absolute(file)],
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if ui::run(forwarded) == gtk::glib::ExitCode::SUCCESS { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
