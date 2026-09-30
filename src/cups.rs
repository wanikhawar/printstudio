//! Direct bindings to the handful of libcups calls we need.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use crate::ppd::{self, PrinterOption};

pub const VIRTUAL_URI_SCHEME: &str = "printstudio:";

#[repr(C)]
struct CupsOption {
    name: *mut c_char,
    value: *mut c_char,
}

#[repr(C)]
struct CupsDest {
    name: *mut c_char,
    instance: *mut c_char,
    is_default: c_int,
    num_options: c_int,
    options: *mut CupsOption,
}

#[repr(C)]
struct CupsJob {
    id: c_int,
    dest: *mut c_char,
    title: *mut c_char,
    user: *mut c_char,
    format: *mut c_char,
    state: c_int,
    size: c_int,
    priority: c_int,
    completed_time: libc::time_t,
    creation_time: libc::time_t,
    processing_time: libc::time_t,
}

const CUPS_WHICHJOBS_ALL: c_int = -1;
const HTTP_STATUS_CONTINUE: c_int = 100;
/// IPP status codes below this are successes.
const IPP_STATUS_ERROR_BAD_REQUEST: c_int = 0x0400;

/// A PDF that's already laid out on the paper. CUPS then skips its pdftopdf
/// stage (which, in libcupsfilters 2.x, drops form XObjects from pages and
/// prints them blank) and goes straight to the printer driver.
pub const FORMAT_PRINT_READY_PDF: &str = "application/vnd.cups-pdf";

#[link(name = "cups")]
unsafe extern "C" {
    fn cupsGetDests(dests: *mut *mut CupsDest) -> c_int;
    fn cupsFreeDests(num_dests: c_int, dests: *mut CupsDest);
    fn cupsGetDefault() -> *const c_char;
    fn cupsGetPPD(name: *const c_char) -> *const c_char;
    fn cupsAddOption(name: *const c_char, value: *const c_char, num_options: c_int, options: *mut *mut CupsOption) -> c_int;
    fn cupsFreeOptions(num_options: c_int, options: *mut CupsOption);
    fn cupsPrintFile(name: *const c_char, filename: *const c_char, title: *const c_char, num_options: c_int, options: *mut CupsOption) -> c_int;
    fn cupsLastErrorString() -> *const c_char;
    fn cupsGetJobs(jobs: *mut *mut CupsJob, name: *const c_char, myjobs: c_int, whichjobs: c_int) -> c_int;
    fn cupsFreeJobs(num_jobs: c_int, jobs: *mut CupsJob);
    fn cupsCreateJob(http: *mut libc::c_void, name: *const c_char, title: *const c_char, num_options: c_int, options: *mut CupsOption) -> c_int;
    fn cupsStartDocument(http: *mut libc::c_void, name: *const c_char, job_id: c_int, docname: *const c_char, format: *const c_char, last_document: c_int) -> c_int;
    fn cupsWriteRequestData(http: *mut libc::c_void, buffer: *const c_char, length: libc::size_t) -> c_int;
    fn cupsFinishDocument(http: *mut libc::c_void, name: *const c_char) -> c_int;
    fn cupsCancelJob(name: *const c_char, job_id: c_int) -> c_int;
}

fn string(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

fn last_error() -> String {
    string(unsafe { cupsLastErrorString() }).unwrap_or_else(|| "unknown CUPS error".into())
}

struct Dest {
    name: String,
    is_default: bool,
    options: HashMap<String, String>,
}

/// Asking CUPS for its destinations takes ~0.2 s (it also browses the
/// network), so do it once per process; a print dialog doesn't live long.
fn dests() -> &'static [Dest] {
    static DESTS: OnceLock<Vec<Dest>> = OnceLock::new();
    DESTS.get_or_init(load_dests)
}

fn load_dests() -> Vec<Dest> {
    let mut raw: *mut CupsDest = ptr::null_mut();
    let n = unsafe { cupsGetDests(&mut raw) };
    let mut out = Vec::new();
    for i in 0..n.max(0) as usize {
        let d = unsafe { &*raw.add(i) };
        if !d.instance.is_null() {
            continue; // lpoptions instances like "Printer/draft"
        }
        let mut options = HashMap::new();
        for j in 0..d.num_options.max(0) as usize {
            let o = unsafe { &*d.options.add(j) };
            if let (Some(k), Some(v)) = (string(o.name), string(o.value)) {
                options.insert(k, v);
            }
        }
        if let Some(name) = string(d.name) {
            out.push(Dest { name, is_default: d.is_default != 0, options });
        }
    }
    unsafe { cupsFreeDests(n, raw) };
    out
}

/// Real printers only: the Print Studio virtual queue is left out.
pub fn printers() -> Vec<String> {
    let mut names: Vec<String> = dests()
        .iter()
        .filter(|d| !d.options.get("device-uri").is_some_and(|u| u.starts_with(VIRTUAL_URI_SCHEME)))
        .map(|d| d.name.clone())
        .collect();
    names.sort();
    names
}

pub fn default_printer(printers: &[String]) -> Option<String> {
    let user_default = dests().iter().find(|d| d.is_default).map(|d| d.name.clone());
    let server_default = string(unsafe { cupsGetDefault() });
    [user_default, server_default]
        .into_iter()
        .flatten()
        .find(|n| printers.contains(n))
        .or_else(|| printers.first().cloned())
}

#[derive(Clone, Debug, Default)]
pub struct PrinterInfo {
    pub name: String,
    pub options: Vec<PrinterOption>,
    pub paper_sizes: HashMap<String, (f64, f64)>,
    pub imageable: HashMap<String, [f64; 4]>,
}

/// Used when the printer doesn't list its printable area (about 1/8 inch).
const DEFAULT_MARGIN: f64 = 9.0;

impl PrinterInfo {
    pub fn paper(&self, page_size: Option<&str>) -> Option<(f64, f64)> {
        let name = page_size?;
        self.paper_sizes.get(name).copied().or_else(|| fallback_paper(name))
    }

    /// The widest unprintable border for a paper size, used on every side
    /// (the paper may go through the printer either way round).
    pub fn margin(&self, page_size: Option<&str>) -> f64 {
        let (Some(name), Some((w, h))) = (page_size, self.paper(page_size)) else { return DEFAULT_MARGIN };
        match self.imageable.get(name) {
            Some([l, b, r, t]) => [*l, *b, w - r, h - t].into_iter().fold(0.0, f64::max).clamp(0.0, 72.0),
            None => DEFAULT_MARGIN,
        }
    }
}

fn fallback_paper(name: &str) -> Option<(f64, f64)> {
    Some(match name {
        "A4" => (595.0, 842.0),
        "A5" => (420.0, 595.0),
        "A6" => (297.0, 420.0),
        "Letter" => (612.0, 792.0),
        "Legal" => (612.0, 1008.0),
        "Executive" => (522.0, 756.0),
        _ => return None,
    })
}

pub fn printer_info(name: &str) -> PrinterInfo {
    let mut info = PrinterInfo { name: name.into(), ..Default::default() };
    let Some(path) = string(unsafe { cupsGetPPD(c(name).as_ptr()) }) else {
        return info; // driverless queue without a PPD
    };
    let text = std::fs::read(&path).map(|b| ppd::decode(&b)).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    let parsed = ppd::parse(&text);
    // Options saved with `lpoptions -p NAME -o key=value` override the PPD defaults.
    let saved = dests().iter().find(|d| d.name == name).map(|d| d.options.clone()).unwrap_or_default();
    info.options = parsed.options;
    for opt in &mut info.options {
        if let Some(v) = saved.get(&opt.keyword).filter(|v| opt.choices.iter().any(|(c, _)| c == *v)) {
            opt.default = v.clone();
        }
    }
    info.paper_sizes = parsed.paper_sizes;
    info.imageable = parsed.imageable;
    info
}

/// Submit a file. With `format` the document type is given explicitly
/// (e.g. `FORMAT_PRINT_READY_PDF`); without it CUPS works it out.
pub fn print_file(printer: &str, path: &Path, title: &str, options: &[(String, String)],
                  format: Option<&str>) -> Result<i32, String> {
    let mut opts: *mut CupsOption = ptr::null_mut();
    let mut n: c_int = 0;
    for (k, v) in options {
        n = unsafe { cupsAddOption(c(k).as_ptr(), c(v).as_ptr(), n, &mut opts) };
    }
    let result = match format {
        None => {
            let file = c(&path.to_string_lossy());
            let id = unsafe { cupsPrintFile(c(printer).as_ptr(), file.as_ptr(), c(title).as_ptr(), n, opts) };
            if id == 0 { Err(last_error()) } else { Ok(id) }
        }
        Some(format) => submit_with_format(printer, path, title, n, opts, format),
    };
    unsafe { cupsFreeOptions(n, opts) };
    result
}

fn submit_with_format(printer: &str, path: &Path, title: &str, n: c_int, opts: *mut CupsOption,
                      format: &str) -> Result<i32, String> {
    let data = std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let (name, title, format) = (c(printer), c(title), c(format));
    let http = ptr::null_mut(); // CUPS_HTTP_DEFAULT
    let id = unsafe { cupsCreateJob(http, name.as_ptr(), title.as_ptr(), n, opts) };
    if id == 0 {
        return Err(last_error());
    }
    let fail = |what: &str| {
        let err = format!("{what}: {}", last_error());
        unsafe { cupsCancelJob(name.as_ptr(), id) };
        Err(err)
    };
    if unsafe { cupsStartDocument(http, name.as_ptr(), id, title.as_ptr(), format.as_ptr(), 1) } != HTTP_STATUS_CONTINUE {
        return fail("Couldn't start the print job");
    }
    for chunk in data.chunks(64 * 1024) {
        if unsafe { cupsWriteRequestData(http, chunk.as_ptr().cast(), chunk.len()) } != HTTP_STATUS_CONTINUE {
            return fail("Couldn't send the document");
        }
    }
    if unsafe { cupsFinishDocument(http, name.as_ptr()) } >= IPP_STATUS_ERROR_BAD_REQUEST {
        return fail("The printer didn't accept the document");
    }
    Ok(id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Held,
    Printing,
    Stopped,
    Cancelled,
    Aborted,
    Done,
    Unknown,
}

impl JobState {
    pub fn is_final(self) -> bool {
        matches!(self, JobState::Cancelled | JobState::Aborted | JobState::Done)
    }

    pub fn describe(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Held => "held",
            JobState::Printing => "printing",
            JobState::Stopped => "stopped",
            JobState::Cancelled => "cancelled",
            JobState::Aborted => "aborted",
            JobState::Done => "done",
            JobState::Unknown => "unknown",
        }
    }
}

pub fn job_state(job_id: i32) -> JobState {
    let mut raw: *mut CupsJob = ptr::null_mut();
    let n = unsafe { cupsGetJobs(&mut raw, ptr::null(), 1, CUPS_WHICHJOBS_ALL) };
    let mut state = JobState::Unknown;
    for i in 0..n.max(0) as usize {
        let job = unsafe { &*raw.add(i) };
        if job.id == job_id {
            state = match job.state {
                3 => JobState::Queued,
                4 => JobState::Held,
                5 => JobState::Printing,
                6 => JobState::Stopped,
                7 => JobState::Cancelled,
                8 => JobState::Aborted,
                9 => JobState::Done,
                _ => JobState::Unknown,
            };
        }
    }
    unsafe { cupsFreeJobs(n, raw) };
    state
}
