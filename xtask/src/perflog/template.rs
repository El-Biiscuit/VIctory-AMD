//! A small pattern language for the runtime's log lines: the literal text of
//! a `tracing::info!` format string with its placeholders named.
//!
//! * `{name}` captures a number (`12.3`, `-0.0`, `inf`, `NaN`).
//! * `{$name}` captures text.
//! * `{=name}` captures text that qualifies the metric keys of the line
//!   (`Direct3D {=queue} queue calls` gives `d3d.title.calls`).
//! * `{_}` skips text up to the next literal (the first occurrence).
//! * `{**}` skips text up to the last occurrence of the next literal.
//!
//! A pattern may start with one number capture: it is the word just before
//! the first literal (`{fps} fps, ...` for a line whose message starts with
//! the number).
//!
//! Whitespace runs count as one space on both sides, so a format string
//! split over source lines matches the line it prints.

/// One piece of a pattern.
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Lit(String),
    Num(String),
    Text(String),
    Qualifier(String),
    Skip,
    SkipLast,
}

/// A compiled pattern: an optional leading number, then a literal (its
/// anchor); never two captures or skips side by side.
#[derive(Debug, Clone)]
pub struct Template {
    lead: Option<String>,
    segs: Vec<Seg>,
}

/// What a line gave: numbers by name, text by name, and qualifiers in order.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Captures {
    pub nums: Vec<(String, f64)>,
    pub texts: Vec<(String, String)>,
    pub qualifiers: Vec<String>,
}

/// Collapses every whitespace run into one space and trims both ends.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

impl Template {
    /// Compiles `pattern`. Panics on a malformed pattern: patterns are
    /// constants of this crate and the catalog test compiles all of them.
    pub fn compile(pattern: &str) -> Template {
        let pattern = normalize(pattern);
        let mut segs = Vec::new();
        let mut rest = pattern.as_str();
        let mut lead = None;
        if rest.starts_with('{')
            && !rest.starts_with("{$")
            && !rest.starts_with("{=")
            && !rest.starts_with("{_")
            && !rest.starts_with("{*")
        {
            let end = rest
                .find('}')
                .unwrap_or_else(|| panic!("unclosed brace in {pattern:?}"));
            lead = Some(rest[1..end].to_owned());
            rest = &rest[end + 1..];
        }
        while !rest.is_empty() {
            match rest.find('{') {
                Some(0) => {
                    let end = rest
                        .find('}')
                        .unwrap_or_else(|| panic!("unclosed brace in {pattern:?}"));
                    let inner = &rest[1..end];
                    let seg = match inner {
                        "_" => Seg::Skip,
                        "**" => Seg::SkipLast,
                        _ if inner.starts_with('$') => Seg::Text(inner[1..].to_owned()),
                        _ if inner.starts_with('=') => Seg::Qualifier(inner[1..].to_owned()),
                        _ => Seg::Num(inner.to_owned()),
                    };
                    if !matches!(segs.last(), Some(Seg::Lit(_))) {
                        panic!("a capture must follow a literal in {pattern:?}");
                    }
                    segs.push(seg);
                    rest = &rest[end + 1..];
                }
                Some(at) => {
                    segs.push(Seg::Lit(rest[..at].to_owned()));
                    rest = &rest[at..];
                }
                None => {
                    segs.push(Seg::Lit(rest.to_owned()));
                    rest = "";
                }
            }
        }
        assert!(
            matches!(segs.first(), Some(Seg::Lit(_))),
            "pattern must start with a literal: {pattern:?}"
        );
        Template { lead, segs }
    }

    /// The first literal: a line holds it or cannot match.
    pub fn anchor(&self) -> &str {
        match &self.segs[0] {
            Seg::Lit(text) => text,
            _ => unreachable!("compile checks the anchor"),
        }
    }

    /// The literal pieces, in order.
    #[cfg(test)]
    pub fn literals(&self) -> impl Iterator<Item = &str> {
        self.segs.iter().filter_map(|s| match s {
            Seg::Lit(text) => Some(text.as_str()),
            _ => None,
        })
    }

    /// The names of the numbers this pattern captures.
    #[cfg(test)]
    pub fn number_names(&self) -> impl Iterator<Item = &str> {
        self.lead
            .iter()
            .map(String::as_str)
            .chain(self.segs.iter().filter_map(|s| match s {
                Seg::Num(name) => Some(name.as_str()),
                _ => None,
            }))
    }

    /// Matches `line` (already [`normalize`]d). Each occurrence of the
    /// anchor is tried in turn; the first that matches wins.
    pub fn matches(&self, line: &str) -> Option<Captures> {
        let anchor = self.anchor();
        let mut from = 0;
        while let Some(at) = line[from..].find(anchor) {
            let start = from + at;
            if let Some(mut c) = self.match_at(line, start + anchor.len()) {
                match &self.lead {
                    None => return Some(c),
                    Some(name) => {
                        let word = line[..start].trim_end().rsplit(' ').next().unwrap_or("");
                        if let Some(v) = parse_number(word) {
                            c.nums.insert(0, (name.clone(), v));
                            return Some(c);
                        }
                    }
                }
            }
            from = start + anchor.len().max(1);
            if from >= line.len() {
                break;
            }
        }
        None
    }

    fn match_at(&self, line: &str, mut pos: usize) -> Option<Captures> {
        let mut caps = Captures::default();
        let mut i = 1;
        while i < self.segs.len() {
            let seg = &self.segs[i];
            if let Seg::Lit(text) = seg {
                // A literal straight after a literal cannot happen (compile
                // merges them), but a literal after the anchor's capture
                // has been consumed already; this branch is a plain check.
                if !line[pos..].starts_with(text.as_str()) {
                    return None;
                }
                pos += text.len();
                i += 1;
                continue;
            }
            let next = match self.segs.get(i + 1) {
                Some(Seg::Lit(text)) => Some(text.as_str()),
                Some(_) => unreachable!("compile forbids adjacent captures"),
                None => None,
            };
            let (value, after) = match next {
                Some(lit) => {
                    let found = if matches!(seg, Seg::SkipLast) {
                        line[pos..].rfind(lit)
                    } else {
                        line[pos..].find(lit)
                    }?;
                    (&line[pos..pos + found], pos + found + lit.len())
                }
                // At the end: a number runs to the next space, text and
                // skips to the end of the line.
                None => match seg {
                    Seg::Num(_) => {
                        let len = line[pos..].find(' ').unwrap_or(line.len() - pos);
                        (&line[pos..pos + len], pos + len)
                    }
                    _ => (&line[pos..], line.len()),
                },
            };
            match seg {
                Seg::Num(name) => caps.nums.push((name.clone(), parse_number(value)?)),
                Seg::Text(name) => caps.texts.push((name.clone(), value.trim().to_owned())),
                Seg::Qualifier(_) => caps.qualifiers.push(slug(value)),
                Seg::Skip | Seg::SkipLast => {}
                Seg::Lit(_) => unreachable!(),
            }
            pos = after;
            i += if next.is_some() { 2 } else { 1 };
        }
        Some(caps)
    }
}

/// A number as Rust's `Display` writes `f64` and integers.
pub fn parse_number(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() || text.contains(' ') {
        return None;
    }
    text.parse::<f64>().ok()
}

/// Lower-case words joined by `_`: `upload and readback` gives
/// `upload_and_readback`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Template, normalize, parse_number, slug};

    #[test]
    fn numbers_between_literals_are_captured() {
        let t = Template::compile("line: {fps} fps, {ms} ms/frame (");
        let c = t
            .matches("INFO x: line: 5.0 fps, 201.8 ms/frame (147.1 preparing")
            .unwrap();
        assert_eq!(c.nums, [("fps".to_owned(), 5.0), ("ms".to_owned(), 201.8)]);
    }

    #[test]
    fn a_leading_number_is_the_word_before_the_anchor() {
        let t = Template::compile("{fps} fps, {ms} ms/frame (");
        let c = t
            .matches("INFO victory_gpu::host: 5.0 fps, 201.8 ms/frame (147.1")
            .unwrap();
        assert_eq!(c.nums, [("fps".to_owned(), 5.0), ("ms".to_owned(), 201.8)]);
        assert!(
            t.matches("INFO victory_gpu::host: many fps, 201.8 ms/frame (")
                .is_none()
        );
    }

    #[test]
    #[should_panic(expected = "a capture must follow a literal")]
    fn two_captures_cannot_be_side_by_side() {
        let _ = Template::compile("a {x}{y}");
    }

    #[test]
    fn captures_numbers_text_and_qualifiers() {
        let t = Template::compile(
            "Direct3D {=queue} queue calls: {calls}/frame, {ms} ms/frame in X, longest call {longest} ms",
        );
        let line = "2026 INFO victory_gpu::host: Direct3D upload and readback queue calls: 12.5/frame, 0.300 ms/frame in X, longest call 1.250 ms";
        let c = t.matches(&normalize(line)).unwrap();
        assert_eq!(c.qualifiers, ["upload_and_readback"]);
        assert_eq!(
            c.nums,
            [
                ("calls".to_owned(), 12.5),
                ("ms".to_owned(), 0.3),
                ("longest".to_owned(), 1.25)
            ]
        );
    }

    #[test]
    fn whitespace_runs_count_as_one_space() {
        let t = Template::compile("heap buffers {p} passes {ms} ms;   texture slots");
        let c = t
            .matches(&normalize(
                "heap buffers 2.0 passes 0.5 ms;                  texture slots",
            ))
            .unwrap();
        assert_eq!(c.nums.len(), 2);
    }

    #[test]
    fn skips_take_the_first_or_the_last_occurrence() {
        let first = Template::compile("a {_}cpu {x}%");
        let last = Template::compile("a {**}cpu {x}%");
        let line = "a b cpu 1% c cpu 2%";
        assert_eq!(first.matches(line).unwrap().nums[0].1, 1.0);
        assert_eq!(last.matches(line).unwrap().nums[0].1, 2.0);
    }

    #[test]
    fn a_later_anchor_is_tried_when_the_first_fails() {
        let t = Template::compile("memory: {mb} MB private");
        let line = "the PC's shared memory: none; memory: 512 MB private";
        assert_eq!(t.matches(line).unwrap().nums[0].1, 512.0);
    }

    #[test]
    fn text_that_is_not_a_number_fails_the_match() {
        let t = Template::compile("x {a} y");
        assert!(t.matches("x abc y").is_none());
        assert!(t.matches("x 1 2 y").is_none());
        assert!(t.matches("x 3 y").is_some());
    }

    #[test]
    fn a_number_at_the_end_runs_to_the_next_space() {
        let t = Template::compile("present token={token}");
        assert_eq!(
            t.matches("DEBUG present token=42 extra").unwrap().nums[0].1,
            42.0
        );
    }

    #[test]
    fn rust_float_spellings_parse() {
        assert_eq!(parse_number("12.3"), Some(12.3));
        assert_eq!(parse_number("-0.0"), Some(0.0));
        assert!(parse_number("inf").unwrap().is_infinite());
        assert!(parse_number("NaN").unwrap().is_nan());
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn slugs_are_lower_case_words() {
        assert_eq!(slug("upload and readback"), "upload_and_readback");
        assert_eq!(slug(" Title "), "title");
    }
}
