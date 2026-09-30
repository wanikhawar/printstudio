//! Which source pages land on which side of which sheet, in what order and in
//! how many passes. Pure index arithmetic, no PDF code, so every ordering rule
//! is easy to test.
//!
//! Order of operations: page ranges -> n-up (pages onto sides) -> odd/even
//! (counted on output sides, like CUPS `page-set`) -> duplex padding ->
//! copies/collate -> reverse.

use serde::{Deserialize, Serialize};

/// Source page indices printed on one side of a sheet. Empty means blank.
pub type Side = Vec<usize>;

pub const NUP_CHOICES: [usize; 6] = [1, 2, 4, 6, 9, 16];

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub cols: usize,
    pub rows: usize,
    /// Composed on a landscape canvas, then turned onto the portrait paper
    /// (what CUPS number-up does too).
    pub landscape: bool,
}

pub fn layout(nup: usize) -> Option<Layout> {
    let (cols, rows, landscape) = match nup {
        1 => (1, 1, false),
        2 => (2, 1, true),
        4 => (2, 2, false),
        6 => (3, 2, true),
        9 => (3, 3, false),
        16 => (4, 4, false),
        _ => return None,
    };
    Some(Layout { cols, rows, landscape })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageSet {
    #[default]
    All,
    Odd,
    Even,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// Each page goes on paper turned the same way as the page.
    #[default]
    Auto,
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scaling {
    /// Grow or shrink each page to fill the printable area.
    #[default]
    Fit,
    /// Only shrink pages that don't fit; smaller ones print at actual size.
    Shrink,
    /// Actual size, centred; anything beyond the paper is cut off.
    Actual,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobOptions {
    /// "1-3,7,10-"; empty means all pages.
    pub page_ranges: String,
    pub page_set: PageSet,
    pub copies: u32,
    pub collate: bool,
    /// Face-up output trays need the last page printed first.
    pub reverse: bool,
    pub nup: usize,
    /// 0 / 90 / 180 / 270, applied to every output side.
    pub rotate: u32,
    pub orientation: Orientation,
    pub scaling: Scaling,
    /// Guided manual two-sided printing.
    pub duplex: bool,
    pub backs_reverse: bool,
    pub backs_rotate: bool,
}

impl Default for JobOptions {
    fn default() -> Self {
        Self {
            page_ranges: String::new(),
            page_set: PageSet::All,
            copies: 1,
            collate: true,
            reverse: true,
            nup: 1,
            rotate: 0,
            orientation: Orientation::Auto,
            scaling: Scaling::Fit,
            duplex: false,
            backs_reverse: false,
            backs_rotate: false,
        }
    }
}

/// One trip of paper through the printer.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    pub sides: Vec<Side>,
    pub rotate180: bool,
    pub label: &'static str,
}

/// Parse "1-3, 7, 10-" into 0-based page indices. Pages past the end are dropped.
pub fn parse_ranges(spec: &str, n_pages: usize) -> Result<Vec<usize>, String> {
    let spec: String = spec.chars().filter(|c| !c.is_whitespace()).collect();
    if spec.is_empty() {
        return Ok((0..n_pages).collect());
    }
    let mut pages = Vec::new();
    for part in spec.split(',').filter(|p| !p.is_empty()) {
        let bad = || format!("Invalid page range: “{part}”");
        let (start, end) = match part.split_once('-') {
            Some((a, b)) => (
                if a.is_empty() { 1 } else { a.parse::<usize>().map_err(|_| bad())? },
                if b.is_empty() { n_pages } else { b.parse::<usize>().map_err(|_| bad())? },
            ),
            None => {
                let p = part.parse::<usize>().map_err(|_| bad())?;
                (p, p)
            }
        };
        if start < 1 || end < start {
            return Err(bad());
        }
        pages.extend((start - 1)..end.min(n_pages));
    }
    Ok(pages)
}

/// 0-based indices -> "1-3, 7". The inverse of `parse_ranges` for sorted input.
pub fn format_ranges(pages: &[usize]) -> String {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end { format!("{}", start + 1) } else { format!("{}-{}", start + 1, end + 1) });
        i += 1;
    }
    parts.join(", ")
}

pub fn plan(n_pages: usize, opts: &JobOptions) -> Result<Vec<Pass>, String> {
    if layout(opts.nup).is_none() {
        return Err(format!("Unsupported pages per sheet: {}", opts.nup));
    }
    let pages = parse_ranges(&opts.page_ranges, n_pages)?;
    let mut sides: Vec<Side> = pages.chunks(opts.nup).map(|c| c.to_vec()).collect();
    sides = match opts.page_set {
        PageSet::All => sides,
        PageSet::Odd => sides.into_iter().step_by(2).collect(),
        PageSet::Even => sides.into_iter().skip(1).step_by(2).collect(),
    };
    if sides.is_empty() {
        return Err("No pages selected".into());
    }

    // A unit is what must stay together when making copies: one side, or a
    // front/back pair when printing two-sided.
    let per_unit = if opts.duplex { 2 } else { 1 };
    if opts.duplex && sides.len() % 2 == 1 {
        sides.push(Vec::new());
    }
    let units: Vec<Vec<Side>> = sides.chunks(per_unit).map(|c| c.to_vec()).collect();
    let copies = opts.copies.max(1) as usize;
    let units: Vec<Vec<Side>> = if opts.collate {
        (0..copies).flat_map(|_| units.iter().cloned()).collect()
    } else {
        units.iter().flat_map(|u| std::iter::repeat_n(u.clone(), copies)).collect()
    };

    if !opts.duplex {
        let mut seq: Vec<Side> = units.into_iter().flatten().collect();
        if opts.reverse {
            seq.reverse();
        }
        return Ok(vec![Pass { sides: seq, rotate180: false, label: "Pages" }]);
    }
    let mut fronts: Vec<Side> = units.iter().map(|u| u[0].clone()).collect();
    let mut backs: Vec<Side> = units.iter().map(|u| u[1].clone()).collect();
    if opts.reverse {
        fronts.reverse();
    }
    if opts.backs_reverse {
        backs.reverse();
    }
    Ok(vec![
        Pass { sides: fronts, rotate180: false, label: "Front" },
        Pass { sides: backs, rotate180: opts.backs_rotate, label: "Back" },
    ])
}

/// Every side of every pass, with the rotation it gets on paper.
pub fn flatten(passes: &[Pass], opts: &JobOptions) -> Vec<(Side, u32)> {
    passes
        .iter()
        .flat_map(|p| {
            let rot = (opts.rotate + if p.rotate180 { 180 } else { 0 }) % 360;
            p.sides.iter().map(move |s| (s.clone(), rot))
        })
        .collect()
}

pub fn describe_side(side: &Side) -> String {
    let nums: Vec<usize> = side.iter().map(|i| i + 1).collect();
    match nums.as_slice() {
        [] => "blank".into(),
        [one] => format!("p. {one}"),
        [first, .., last] if nums.windows(2).all(|w| w[1] == w[0] + 1) => format!("pp. {first}–{last}"),
        _ => format!("pp. {}", nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",")),
    }
}

/// Thumbnail captions for the print preview, in print order.
pub fn side_labels(passes: &[Pass], duplex: bool) -> Vec<String> {
    passes
        .iter()
        .flat_map(|p| {
            p.sides.iter().enumerate().map(move |(n, side)| {
                let prefix = if duplex { format!("{} {}", p.label, n + 1) } else { format!("{}", n + 1) };
                format!("{prefix} · {}", describe_side(side))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> JobOptions {
        JobOptions { reverse: false, ..Default::default() }
    }

    fn order(passes: &[Pass], i: usize) -> Vec<Vec<usize>> {
        passes[i].sides.iter().map(|s| s.iter().map(|p| p + 1).collect()).collect()
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_ranges("1-2, 5, 8-", 9).unwrap(), vec![0, 1, 4, 7, 8]);
        assert_eq!(parse_ranges("-2,4-99", 5).unwrap(), vec![0, 1, 3, 4]);
        for bad in ["x", "3-1", "0", "1-a"] {
            assert!(parse_ranges(bad, 5).is_err(), "{bad}");
        }
    }

    #[test]
    fn format_round_trip() {
        assert_eq!(format_ranges(&[0, 1, 2, 6, 8, 9]), "1-3, 7, 9-10");
        assert_eq!(format_ranges(&[]), "");
        assert_eq!(parse_ranges(&format_ranges(&[4, 0, 1]), 9).unwrap(), vec![0, 1, 4]);
    }

    #[test]
    fn plain_reverse_odd_even() {
        assert_eq!(order(&plan(3, &opts()).unwrap(), 0), vec![vec![1], vec![2], vec![3]]);
        let o = JobOptions { reverse: true, ..opts() };
        assert_eq!(order(&plan(3, &o).unwrap(), 0), vec![vec![3], vec![2], vec![1]]);
        let o = JobOptions { page_set: PageSet::Odd, ..opts() };
        assert_eq!(order(&plan(5, &o).unwrap(), 0), vec![vec![1], vec![3], vec![5]]);
        let o = JobOptions { page_set: PageSet::Even, ..opts() };
        assert_eq!(order(&plan(5, &o).unwrap(), 0), vec![vec![2], vec![4]]);
    }

    #[test]
    fn collate() {
        let o = JobOptions { copies: 2, ..opts() };
        assert_eq!(order(&plan(2, &o).unwrap(), 0), vec![vec![1], vec![2], vec![1], vec![2]]);
        let o = JobOptions { copies: 2, collate: false, ..opts() };
        assert_eq!(order(&plan(2, &o).unwrap(), 0), vec![vec![1], vec![1], vec![2], vec![2]]);
        let o = JobOptions { copies: 2, reverse: true, ..opts() };
        assert_eq!(order(&plan(2, &o).unwrap(), 0), vec![vec![2], vec![1], vec![2], vec![1]]);
    }

    #[test]
    fn nup_and_errors() {
        let o = JobOptions { nup: 2, ..opts() };
        assert_eq!(order(&plan(5, &o).unwrap(), 0), vec![vec![1, 2], vec![3, 4], vec![5]]);
        let o = JobOptions { page_ranges: "7-9".into(), ..opts() };
        assert!(plan(3, &o).is_err());
    }

    #[test]
    fn duplex() {
        let o = JobOptions { duplex: true, ..opts() };
        let p = plan(5, &o).unwrap();
        assert_eq!(order(&p, 0), vec![vec![1], vec![3], vec![5]]);
        assert_eq!(order(&p, 1), vec![vec![2], vec![4], vec![]]);

        let o = JobOptions { duplex: true, reverse: true, ..opts() };
        let p = plan(4, &o).unwrap();
        assert_eq!(order(&p, 0), vec![vec![3], vec![1]]);
        assert_eq!(order(&p, 1), vec![vec![2], vec![4]]);

        let o = JobOptions { duplex: true, backs_reverse: true, backs_rotate: true, ..opts() };
        let p = plan(4, &o).unwrap();
        assert_eq!(order(&p, 1), vec![vec![4], vec![2]]);
        assert!(p[1].rotate180);

        let o = JobOptions { duplex: true, copies: 2, ..opts() };
        let p = plan(3, &o).unwrap();
        assert_eq!(order(&p, 0), vec![vec![1], vec![3], vec![1], vec![3]]);
        assert_eq!(order(&p, 1), vec![vec![2], vec![], vec![2], vec![]]);

        let o = JobOptions { duplex: true, copies: 2, collate: false, ..opts() };
        let p = plan(4, &o).unwrap();
        assert_eq!(order(&p, 0), vec![vec![1], vec![1], vec![3], vec![3]]);
        assert_eq!(order(&p, 1), vec![vec![2], vec![2], vec![4], vec![4]]);
    }

    #[test]
    fn describe() {
        assert_eq!(describe_side(&vec![0, 1, 2, 3]), "pp. 1–4");
        assert_eq!(describe_side(&vec![]), "blank");
        assert_eq!(describe_side(&vec![0, 4]), "pp. 1,5");
    }

    #[test]
    fn reads_settings_saved_by_earlier_versions() {
        let json = r#"{"backs_reverse":false,"backs_rotate":true,"collate":true,"duplex":false,
            "fit_to_page":false,"nup":2,"page_set":"odd","reverse":true,"rotate":0}"#;
        let o: JobOptions = serde_json::from_str(json).unwrap();
        assert_eq!((o.nup, o.page_set, o.backs_rotate, o.copies), (2, PageSet::Odd, true, 1));
    }
}
