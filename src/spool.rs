//! Jobs captured by the "PrintStudio" CUPS queue.
//!
//! The backend writes `job-<id>.json` and then `job-<id>.pdf` into the user's
//! spool directory. The PDF is written last, so once it exists the job is complete.

use std::ffi::CStr;
use std::path::{Path, PathBuf};

pub const SPOOL_ROOT: &str = "/var/spool/printstudio";

pub fn username() -> String {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_name.is_null() {
            return CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned();
        }
    }
    std::env::var("USER").unwrap_or_default()
}

pub fn spool_dir() -> PathBuf {
    match std::env::var_os("PRINTSTUDIO_SPOOL") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(SPOOL_ROOT).join(username()),
    }
}

#[derive(Clone, Debug)]
pub struct SpoolJob {
    pub pdf: PathBuf,
    pub title: String,
    pub copies: u32,
}

impl SpoolJob {
    pub fn load(pdf: &Path) -> SpoolJob {
        let meta: serde_json::Value = std::fs::read_to_string(pdf.with_extension("json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let copies = match &meta["copies"] {
            serde_json::Value::String(s) => s.trim().parse().unwrap_or(1),
            v => v.as_u64().unwrap_or(1) as u32,
        };
        SpoolJob {
            pdf: pdf.to_path_buf(),
            title: meta["title"].as_str().unwrap_or("").to_string(),
            copies: copies.max(1),
        }
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.pdf);
        let _ = std::fs::remove_file(self.pdf.with_extension("json"));
    }
}
