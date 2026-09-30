//! printstudio [FILE]            open a document
//! printstudio --spool-job JOB    open a job captured by the PrintStudio printer
//! printstudio watch              background service that does the above

mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use printstudio::spool::SpoolJob;
use printstudio::watch;

const USAGE: &str = "Usage: printstudio [FILE]\n       printstudio watch\n\nOne print dialog for every app.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (file, spool) = match args.as_slice() {
        [] => (None, None),
        [cmd] if cmd == "watch" => watch::run(),
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        [flag, job] if flag == "--spool-job" => (None, Some(SpoolJob::load(&PathBuf::from(job)))),
        [file] if !file.starts_with('-') => (Some(PathBuf::from(file)), None),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if ui::run(file, spool) == gtk::glib::ExitCode::SUCCESS { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
