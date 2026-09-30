//! CUPS backend for the "PrintStudio" virtual printer.
//!
//! CUPS runs this as root (it's installed 0700 root). It creates the user's
//! spool directory, drops to that user, and saves the job as
//! /var/spool/printstudio/<user>/job-<id>.pdf. `printstudio watch` then opens
//! the Print Studio window for it.

use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{PermissionsExt, chown};
use std::path::Path;
use std::process::ExitCode;

const OK: u8 = 0; // CUPS_BACKEND_OK
const FAILED: u8 = 1; // CUPS_BACKEND_FAILED

fn log(level: &str, message: &str) {
    eprintln!("{level}: {message}");
}

fn json_string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct User {
    uid: libc::uid_t,
    gid: libc::gid_t,
}

fn lookup_user(name: &str) -> Option<User> {
    let cname = CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let pw = unsafe { &*pw };
    // Make sure the name round-trips (no odd characters slipping into paths).
    let back = unsafe { CStr::from_ptr(pw.pw_name) }.to_str().ok()?;
    (back == name && !name.contains('/')).then_some(User { uid: pw.pw_uid, gid: pw.pw_gid })
}

fn drop_privileges(user: &User) -> io::Result<()> {
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0
            || libc::setgid(user.gid) != 0
            || libc::setuid(user.uid) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    let [_, job_id, user, title, copies, options, rest @ ..] = args else {
        return Err("Usage: printstudio job-id user title copies options [file]".into());
    };
    let job_id: u64 = job_id.parse().map_err(|_| format!("bad job id {job_id:?}"))?;
    let pw = lookup_user(user).ok_or_else(|| format!("Print Studio only accepts jobs from local users (got {user:?})"))?;

    let root = std::env::var("PRINTSTUDIO_SPOOL_ROOT").unwrap_or_else(|_| "/var/spool/printstudio".into());
    let as_root = unsafe { libc::geteuid() } == 0;
    if !as_root && unsafe { libc::geteuid() } != pw.uid {
        return Err("the backend must run as root (install it with mode 0700)".into());
    }

    let user_dir = Path::new(&root).join(user);
    fs::create_dir_all(&root).map_err(|e| format!("can't create {root}: {e}"))?;
    if user_dir.is_symlink() {
        return Err(format!("{} is a symlink, refusing to use it", user_dir.display()));
    }
    fs::create_dir_all(&user_dir).map_err(|e| format!("can't create {}: {e}", user_dir.display()))?;
    if as_root {
        chown(&user_dir, Some(pw.uid), Some(pw.gid)).map_err(|e| e.to_string())?;
    }
    fs::set_permissions(&user_dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;

    let mut input: Box<dyn io::Read> = match rest.first() {
        Some(file) => Box::new(File::open(file).map_err(|e| format!("can't read {file}: {e}"))?),
        None => Box::new(io::stdin().lock()),
    };

    // Everything from here on runs as the user who printed, and stays private to them.
    unsafe { libc::umask(0o077) };
    if as_root {
        drop_privileges(&pw).map_err(|e| format!("can't switch to user {user}: {e}"))?;
    }

    let base = user_dir.join(format!("job-{job_id}"));
    let meta = format!(
        "{{\"title\": {}, \"copies\": {}, \"options\": {}, \"job_id\": \"{job_id}\"}}",
        json_string(title), json_string(copies), json_string(options)
    );
    fs::write(base.with_extension("json"), meta).map_err(|e| e.to_string())?;
    let part = base.with_extension("part");
    let mut out = File::create(&part).map_err(|e| e.to_string())?;
    io::copy(&mut input, &mut out).map_err(|e| format!("copying the job failed: {e}"))?;
    out.flush().map_err(|e| e.to_string())?;
    // The watcher only looks at *.pdf, so the job appears all at once.
    fs::rename(&part, base.with_extension("pdf")).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 {
        // Device discovery (lpinfo -v)
        println!("direct printstudio \"Unknown\" \"Print Studio (opens your print dialog)\"");
        return ExitCode::from(OK);
    }
    match run(&args) {
        Ok(()) => {
            log("INFO", "Sent to Print Studio");
            ExitCode::from(OK)
        }
        Err(e) => {
            log("ERROR", &e);
            ExitCode::from(FAILED)
        }
    }
}
