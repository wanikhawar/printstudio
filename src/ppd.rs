//! Just enough of a PPD parser to list a printer's options, their choices and
//! defaults, and its paper sizes.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct PrinterOption {
    pub keyword: String,
    pub label: String,
    pub group: String,
    pub default: String,
    /// (value, label)
    pub choices: Vec<(String, String)>,
    /// Shown under "Printer settings" rather than tucked away in "Advanced".
    pub common: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Ppd {
    pub options: Vec<PrinterOption>,
    pub paper_sizes: HashMap<String, (f64, f64)>,
    /// Printable area per paper size: left, bottom, right, top, in points.
    pub imageable: HashMap<String, [f64; 4]>,
}

/// Duplicate of PageSize that every PPD carries.
const HIDDEN: [&str; 1] = ["PageRegion"];
const COMMON_KEYWORDS: [&str; 7] = ["PageSize", "MediaType", "InputSlot", "ColorModel", "Resolution", "cupsPrintQuality", "OutputMode"];
const COMMON_WORDS: [&str; 11] = [
    "quality", "resolution", "color model", "colour model", "colormodel", "grayscale", "greyscale",
    "monocolor", "mono color", "media type", "mediatype",
];

fn is_common(keyword: &str, label: &str) -> bool {
    let text = format!("{keyword} {label}").to_lowercase();
    COMMON_KEYWORDS.contains(&keyword) || COMMON_WORDS.iter().any(|w| text.contains(w))
}

/// "Name/Label" -> ("Name", "Label"); the label defaults to the name.
fn name_and_label(spec: &str) -> (String, String) {
    match spec.split_once('/') {
        Some((name, label)) if !label.trim().is_empty() => (name.trim().into(), label.trim().into()),
        Some((name, _)) => (name.trim().into(), name.trim().into()),
        None => (spec.trim().into(), spec.trim().into()),
    }
}

/// PPDs are ISO-8859-1 by default.
pub fn decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

pub fn parse(text: &str) -> Ppd {
    let mut ppd = Ppd::default();
    let mut defaults: HashMap<String, String> = HashMap::new();
    let mut group: Option<String> = None;
    let mut current: Option<PrinterOption> = None;
    let mut in_quote = false;

    for raw in text.lines() {
        if in_quote {
            if raw.contains('"') {
                in_quote = false;
            }
            continue;
        }
        let line = raw.trim_end();
        if !line.starts_with('*') || line.starts_with("*%") {
            continue;
        }
        if line.matches('"').count() % 2 == 1 {
            in_quote = true; // value continues on the next lines
        }
        let (head, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.trim();
        let (keyword, spec) = head[1..].split_once(char::is_whitespace).unwrap_or((&head[1..], ""));
        let spec = spec.trim();

        match keyword {
            "OpenGroup" => group = Some(name_and_label(value).1),
            "CloseGroup" => group = None,
            "OpenUI" | "JCLOpenUI" => {
                let (kw, label) = name_and_label(spec.trim_start_matches('*'));
                current = Some(PrinterOption {
                    common: is_common(&kw, &label),
                    keyword: kw,
                    label,
                    group: group.clone().unwrap_or_else(|| "General".into()),
                    default: String::new(),
                    choices: Vec::new(),
                });
            }
            "CloseUI" | "JCLCloseUI" => {
                if let Some(opt) = current.take()
                    && !opt.choices.is_empty()
                    && !HIDDEN.contains(&opt.keyword.as_str())
                {
                    ppd.options.push(opt);
                }
            }
            "ImageableArea" => {
                let (name, _) = name_and_label(spec);
                let nums: Vec<f64> = value.trim_matches('"').split_whitespace().filter_map(|n| n.parse().ok()).collect();
                if let [l, b, r, t, ..] = nums.as_slice() {
                    ppd.imageable.insert(name, [*l, *b, *r, *t]);
                }
            }
            "PaperDimension" => {
                let (name, _) = name_and_label(spec);
                let nums: Vec<f64> = value.trim_matches('"').split_whitespace().filter_map(|n| n.parse().ok()).collect();
                if let [w, h, ..] = nums.as_slice() {
                    ppd.paper_sizes.insert(name, (*w, *h));
                }
            }
            kw if kw.starts_with("Default") && spec.is_empty() => {
                if let Some(v) = value.split_whitespace().next() {
                    defaults.insert(kw["Default".len()..].to_string(), v.to_string());
                }
            }
            kw => {
                if let Some(opt) = current.as_mut().filter(|o| o.keyword == kw && !spec.is_empty()) {
                    opt.choices.push(name_and_label(spec));
                }
            }
        }
    }

    for opt in &mut ppd.options {
        opt.default = defaults
            .get(&opt.keyword)
            .filter(|d| opt.choices.iter().any(|(v, _)| v == *d))
            .cloned()
            .unwrap_or_else(|| opt.choices[0].0.clone());
    }
    ppd
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"*PPD-Adobe: "4.3"
*% comment
*OpenGroup: General/General
*OpenUI *PageSize/Media Size: PickOne
*DefaultPageSize: Letter
*PageSize A4/A4: "<</PageSize[595 842]>>setpagedevice"
*PageSize Letter/US Letter: "<</PageSize[612 792]
>>setpagedevice"
*CloseUI: *PageSize
*OpenUI *PageRegion: PickOne
*PageRegion A4/A4: "x"
*CloseUI: *PageRegion
*CloseGroup: General
*OpenGroup: Print Settings (Advanced)
*OpenUI *BRResolution/Print Quality: PickOne
*DefaultBRResolution: PlainNormal
*BRResolution PlainFast/Plain Fast: ""
*BRResolution PlainNormal/Plain Normal: ""
*CloseUI: *BRResolution
*OpenUI *BRBiDir/Bi-Directional Printing: PickOne
*DefaultBRBiDir: ON
*BRBiDir OFF/Off: ""
*BRBiDir ON/On: ""
*CloseUI: *BRBiDir
*CloseGroup: Print Settings (Advanced)
*PaperDimension A4/A4:										"595 842"
*PaperDimension Letter/US Letter: "612 792"
*ImageableArea A4/A4:								"9 9 586 833"
"#;

    #[test]
    fn parses_options_groups_defaults_and_paper() {
        let ppd = parse(SAMPLE);
        let kws: Vec<_> = ppd.options.iter().map(|o| o.keyword.as_str()).collect();
        assert_eq!(kws, ["PageSize", "BRResolution", "BRBiDir"]);
        let size = &ppd.options[0];
        assert_eq!((size.default.as_str(), size.choices.len(), size.common), ("Letter", 2, true));
        assert_eq!(size.choices[1], ("Letter".to_string(), "US Letter".to_string()));
        let quality = &ppd.options[1];
        assert_eq!((quality.group.as_str(), quality.label.as_str(), quality.common), ("Print Settings (Advanced)", "Print Quality", true));
        assert!(!ppd.options[2].common);
        assert_eq!(ppd.paper_sizes["A4"], (595.0, 842.0));
        assert_eq!(ppd.paper_sizes["Letter"], (612.0, 792.0));
        assert_eq!(ppd.imageable["A4"], [9.0, 9.0, 586.0, 833.0]);
    }
}
