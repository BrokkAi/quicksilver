//! Capture a mutant from an edited checkout.
//!
//! The model (or a person) edits function bodies in its checkout. QS never
//! compiles that checkout. Instead it checks that every change lies inside a
//! function body the map knows, and lifts each changed body out as the
//! replacement for that site. The check is a byte-anchored split against the
//! baseline: the text outside every body must appear verbatim, in order, in
//! the edited file; whatever lies between two such anchors is the new body.

use crate::sitemap::{tracked_file_hashes, SiteMap};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// One function body in the baseline version of a file: the byte range of
/// the braces, inclusive of both.
#[derive(Debug, Clone)]
pub struct BodyRange {
    pub site_id: u32,
    pub fqn: String,
    pub start: usize,
    pub end: usize,
}

/// Why an edited file cannot be captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// Text outside every body differs. `line` is 1-based; `in_edited` says
    /// whether it refers to the edited file (exact) or to the baseline file
    /// (the start of the unchanged text that could not be found). `near`
    /// names the surrounding functions.
    OutsideBody {
        line: usize,
        in_edited: bool,
        near: String,
    },
    /// A new body does not start with `{`, end with `}`, or balance braces.
    Unbalanced { fqn: String },
    /// The body ranges do not describe the baseline file (a stale map).
    BadRanges(String),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::OutsideBody {
                line,
                in_edited,
                near,
            } => write!(
                f,
                "edit outside a function body {near} (line {line} of the {})",
                if *in_edited { "edited file" } else { "baseline file" }
            ),
            Rejection::Unbalanced { fqn } => write!(
                f,
                "the new body of `{fqn}` does not start with `{{`, end with `}}` and balance its braces"
            ),
            Rejection::BadRanges(why) => write!(f, "the map does not match this file: {why}"),
        }
    }
}

/// Split `edited` against `original` at the body ranges. Returns the
/// (site id, new body) pairs whose body text differs from the original.
pub fn split_bodies(
    original: &str,
    edited: &str,
    bodies: &[BodyRange],
) -> Result<Vec<(u32, String)>, Rejection> {
    let mut bodies: Vec<&BodyRange> = bodies.iter().collect();
    bodies.sort_by_key(|b| b.start);
    let mut prev_end = 0usize;
    for b in &bodies {
        if b.start < prev_end || b.end <= b.start || b.end > original.len() {
            return Err(Rejection::BadRanges(format!(
                "body of `{}` at bytes {}..{} is out of order or out of range",
                b.fqn, b.start, b.end
            )));
        }
        if original.as_bytes()[b.start] != b'{' || original.as_bytes()[b.end - 1] != b'}' {
            return Err(Rejection::BadRanges(format!(
                "body of `{}` at bytes {}..{} is not brace-delimited",
                b.fqn, b.start, b.end
            )));
        }
        prev_end = b.end;
    }

    let mut out = Vec::new();
    let mut pos = 0usize; // cursor in `edited`
    let mut prev_end = 0usize; // cursor in `original`
    for (i, b) in bodies.iter().enumerate() {
        let seg = &original[prev_end..b.start];
        let near = match i {
            0 => format!("before `{}`", b.fqn),
            _ => format!("between `{}` and `{}`", bodies[i - 1].fqn, b.fqn),
        };
        let off = if i == 0 {
            if edited.starts_with(seg) {
                0
            } else {
                return Err(Rejection::OutsideBody {
                    line: line_of(edited, common_prefix(seg, edited)),
                    in_edited: true,
                    near,
                });
            }
        } else {
            match edited[pos..].find(seg) {
                Some(off) => off,
                None => {
                    return Err(Rejection::OutsideBody {
                        line: line_of(original, prev_end),
                        in_edited: false,
                        near,
                    })
                }
            }
        };
        if i > 0 {
            let prev = bodies[i - 1];
            push_if_changed(&mut out, prev, original, &edited[pos..pos + off])?;
        }
        pos += off + seg.len();
        prev_end = b.end;
    }

    // The tail after the last body must close the edited file exactly.
    let seg = &original[prev_end..];
    let near = match bodies.last() {
        Some(b) => format!("after `{}`", b.fqn),
        None => "with no function in this file".to_string(),
    };
    if !edited[pos..].ends_with(seg) || edited.len() - seg.len() < pos {
        // Locate the first differing byte from the end.
        let tail = &edited[pos..];
        let matched = common_suffix(seg, tail);
        return Err(Rejection::OutsideBody {
            line: line_of(edited, edited.len() - matched),
            in_edited: true,
            near,
        });
    }
    if let Some(last) = bodies.last() {
        let body = &edited[pos..edited.len() - seg.len()];
        push_if_changed(&mut out, last, original, body)?;
    }
    Ok(out)
}

fn push_if_changed(
    out: &mut Vec<(u32, String)>,
    b: &BodyRange,
    original: &str,
    body: &str,
) -> Result<(), Rejection> {
    if body == &original[b.start..b.end] {
        return Ok(());
    }
    if !braces_balanced(body) {
        return Err(Rejection::Unbalanced { fqn: b.fqn.clone() });
    }
    out.push((b.site_id, body.to_string()));
    Ok(())
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

fn common_suffix(a: &str, b: &str) -> usize {
    a.bytes()
        .rev()
        .zip(b.bytes().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

/// 1-based line of byte offset `at`.
pub fn line_of(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text.as_bytes()[..at]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// True when `body` starts with `{`, ends with `}`, and the braces outside
/// string literals, character literals and comments balance without the
/// first brace closing before the last byte.
pub fn braces_balanced(body: &str) -> bool {
    let b = body.as_bytes();
    if b.first() != Some(&b'{') || b.last() != Some(&b'}') {
        return false;
    }
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                if depth == 0 && i + 1 != b.len() {
                    return false;
                }
            }
            b'"' => i = skip_string(b, i),
            b'r' if b[i..].starts_with(b"r\"") || b[i..].starts_with(b"r#") => {
                if let Some(end) = skip_raw_string(b, i) {
                    i = end;
                    continue;
                }
            }
            b'b' if b[i..].starts_with(b"b\"") => i = skip_string(b, i + 1),
            b'b' if b[i..].starts_with(b"br\"") || b[i..].starts_with(b"br#") => {
                if let Some(end) = skip_raw_string(b, i + 1) {
                    i = end;
                    continue;
                }
            }
            b'\'' => {
                // A char literal is `'x'` or `'\..'`; anything else is a lifetime.
                if let Some(end) = skip_char_literal(b, i) {
                    i = end;
                    continue;
                }
            }
            b'/' if b[i..].starts_with(b"//") => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b[i..].starts_with(b"/*") => {
                let mut level = 0usize;
                while i < b.len() {
                    if b[i..].starts_with(b"/*") {
                        level += 1;
                        i += 2;
                    } else if b[i..].starts_with(b"*/") {
                        level -= 1;
                        i += 2;
                        if level == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    depth == 0
}

/// Index of the closing quote of the string that opens at `b[at] == b'"'`.
fn skip_string(b: &[u8], at: usize) -> usize {
    let mut i = at + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i,
            _ => i += 1,
        }
    }
    b.len()
}

/// Index after the raw string that opens at `b[at] == b'r'`, or `None`.
fn skip_raw_string(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    let mut hashes = 0usize;
    while i < b.len() && b[i] == b'#' {
        hashes += 1;
        i += 1;
    }
    if b.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < b.len() {
        if b[i] == b'"'
            && b[i + 1..]
                .iter()
                .take(hashes)
                .filter(|c| **c == b'#')
                .count()
                == hashes
        {
            return Some(i + 1 + hashes);
        }
        i += 1;
    }
    Some(b.len())
}

/// Index after a char literal opening at `b[at] == b'\''`, or `None` for a
/// lifetime or label.
fn skip_char_literal(b: &[u8], at: usize) -> Option<usize> {
    let rest = &b[at + 1..];
    if rest.first() == Some(&b'\\') {
        // Escaped: find the closing quote.
        let mut i = 1;
        while i < rest.len() {
            if rest[i] == b'\'' {
                return Some(at + 1 + i + 1);
            }
            i += 1;
        }
        return None;
    }
    // One UTF-8 character then a quote.
    let s = std::str::from_utf8(rest).ok()?;
    let mut chars = s.char_indices();
    let (_, c) = chars.next()?;
    let next = at + 1 + c.len_utf8();
    (b.get(next) == Some(&b'\'')).then_some(next + 1)
}

/// One changed body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BodyEdit {
    pub site_id: u32,
    pub package: String,
    pub path: String,
    pub fqn: String,
    /// The new body, braces included.
    pub body: String,
}

/// A mutant captured from the checkout: every changed body, switched on
/// together.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditedMutant {
    pub id: String,
    pub edits: Vec<BodyEdit>,
}

impl EditedMutant {
    pub fn fqns(&self) -> String {
        self.edits
            .iter()
            .map(|e| e.fqn.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// The id is a hash of the sorted (site, body) pairs, so the same edit
/// captured twice has the same id.
pub fn mutant_id(edits: &[BodyEdit]) -> String {
    let mut pairs: Vec<(u32, &str)> = edits.iter().map(|e| (e.site_id, e.body.as_str())).collect();
    pairs.sort();
    let mut h = Sha256::new();
    for (site, body) in pairs {
        h.update(site.to_le_bytes());
        h.update(body.as_bytes());
        h.update([0]);
    }
    crate::util::hex(&h.finalize())[..16].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capture {
    Mutant(EditedMutant),
    /// No tracked file differs from the map.
    Nothing,
    /// One message per problem.
    Rejected(Vec<String>),
}

/// Compare the checkout with the files the map was built from and lift the
/// changed bodies out. `baseline` is the scratch copy the map was built from.
pub fn capture(map: &SiteMap, baseline: &Path, checkout: &Path) -> Result<Capture> {
    let now = tracked_file_hashes(checkout)?;
    let mut problems = Vec::new();
    let mut changed: Vec<&String> = Vec::new();
    for (rel, hash) in &now {
        match map.files.get(rel) {
            Some(was) if was == hash => {}
            Some(_) => changed.push(rel),
            None => problems.push(format!(
                "`{rel}` was added; a mutant may only change function bodies"
            )),
        }
    }
    for rel in map.files.keys() {
        if !now.contains_key(rel) {
            problems.push(format!(
                "`{rel}` was removed; a mutant may only change function bodies"
            ));
        }
    }
    let mut by_file: BTreeMap<&str, Vec<BodyRange>> = BTreeMap::new();
    for s in &map.sites {
        by_file.entry(s.path.as_str()).or_default().push(BodyRange {
            site_id: s.site_id,
            fqn: s.fqn.clone(),
            start: s.body_start_byte,
            end: s.body_end_byte,
        });
    }
    let mut edits = Vec::new();
    for rel in changed {
        let Some(ranges) = by_file.get(rel.as_str()) else {
            let dropped: Vec<&str> = map
                .dropped
                .iter()
                .filter(|(path, _, _)| path == rel)
                .map(|(_, fqn, _)| fqn.as_str())
                .collect();
            if dropped.is_empty() {
                problems.push(format!(
                    "`{rel}` changed; it has no mutable function (test code and files without function bodies are never mutated)"
                ));
            } else {
                problems.push(format!(
                    "`{rel}` changed; its functions have no mutation site ({}); see `dropped` in the map for why",
                    dropped.join(", ")
                ));
            }
            continue;
        };
        let original = std::fs::read_to_string(baseline.join(rel))
            .with_context(|| format!("read baseline {rel}"))?;
        let edited = std::fs::read_to_string(checkout.join(rel))
            .with_context(|| format!("read checkout {rel}"))?;
        match split_bodies(&original, &edited, ranges) {
            Ok(bodies) => {
                for (site_id, body) in bodies {
                    let site = map
                        .sites
                        .iter()
                        .find(|s| s.site_id == site_id)
                        .expect("site from the same map");
                    edits.push(BodyEdit {
                        site_id,
                        package: site.package.clone(),
                        path: site.path.clone(),
                        fqn: site.fqn.clone(),
                        body,
                    });
                }
            }
            Err(why) => problems.push(format!("`{rel}`: {why}")),
        }
    }
    if !problems.is_empty() {
        return Ok(Capture::Rejected(problems));
    }
    if edits.is_empty() {
        return Ok(Capture::Nothing);
    }
    edits.sort_by_key(|e| (e.path.clone(), e.site_id));
    Ok(Capture::Mutant(EditedMutant {
        id: mutant_id(&edits),
        edits,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "\
//! doc
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn one() -> u32 { 1 }

impl Thing {
    pub fn name(&self) -> String {
        format!(\"{}\", self.n)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() { assert_eq!(super::add(1, 2), 3); }
}
";

    fn ranges() -> Vec<BodyRange> {
        let body = |fqn: &str, open: &str, close: &str| {
            let start = SRC.find(open).unwrap() + open.len() - 1;
            let end = SRC[start..].find(close).unwrap() + start + close.len();
            BodyRange {
                site_id: fqn.len() as u32,
                fqn: fqn.into(),
                start,
                end,
            }
        };
        vec![
            body("add", "-> i32 {", "\n}"),
            body("one", "-> u32 {", "}"),
            body("Thing::name", "-> String {", "\n    }"),
        ]
    }

    #[test]
    fn ranges_describe_the_fixture() {
        let r = ranges();
        assert_eq!(&SRC[r[0].start..r[0].end], "{\n    a + b\n}");
        assert_eq!(&SRC[r[1].start..r[1].end], "{ 1 }");
        assert_eq!(
            &SRC[r[2].start..r[2].end],
            "{\n        format!(\"{}\", self.n)\n    }"
        );
    }

    #[test]
    fn an_unchanged_file_yields_no_bodies() {
        assert_eq!(split_bodies(SRC, SRC, &ranges()), Ok(vec![]));
    }

    #[test]
    fn a_one_line_body_edit_is_captured_without_touching_the_signature() {
        let edited = SRC.replace("-> u32 { 1 }", "-> u32 { 2 }");
        assert_eq!(
            split_bodies(SRC, &edited, &ranges()),
            Ok(vec![(3, "{ 2 }".into())])
        );
    }

    #[test]
    fn two_edited_bodies_come_back_in_source_order() {
        let edited = SRC
            .replace("    a + b\n", "    let x = a;\n    x - b\n")
            .replace("format!(\"{}\", self.n)", "String::new()");
        assert_eq!(
            split_bodies(SRC, &edited, &ranges()),
            Ok(vec![
                (3, "{\n    let x = a;\n    x - b\n}".into()),
                (11, "{\n        String::new()\n    }".into()),
            ])
        );
    }

    #[test]
    fn an_edited_signature_is_rejected_naming_the_function() {
        let edited = SRC.replace(
            "pub fn add(a: i32, b: i32) -> i32",
            "pub fn add(a: i64, b: i64) -> i64",
        );
        let err = split_bodies(SRC, &edited, &ranges()).unwrap_err();
        assert_eq!(
            err,
            Rejection::OutsideBody {
                line: 2,
                in_edited: true,
                near: "before `add`".into()
            }
        );
        let edited = SRC.replace("pub fn one()", "pub fn one(x: u8)");
        let err = split_bodies(SRC, &edited, &ranges()).unwrap_err();
        assert_eq!(
            err,
            Rejection::OutsideBody {
                line: 4,
                in_edited: false,
                near: "between `add` and `one`".into()
            }
        );
    }

    #[test]
    fn an_edit_in_the_test_module_is_rejected() {
        let edited = SRC.replace("3); }", "4); }");
        let err = split_bodies(SRC, &edited, &ranges()).unwrap_err();
        assert_eq!(
            err,
            Rejection::OutsideBody {
                line: 17,
                in_edited: true,
                near: "after `Thing::name`".into()
            }
        );
    }

    #[test]
    fn a_new_function_after_the_last_body_is_rejected() {
        let edited = format!("{SRC}\npub fn extra() {{}}\n");
        assert!(matches!(
            split_bodies(SRC, &edited, &ranges()),
            Err(Rejection::OutsideBody { .. })
        ));
    }

    #[test]
    fn a_body_that_closes_early_is_unbalanced() {
        let edited = SRC.replace("    a + b\n}", "    a }\n fn evil() { b\n}");
        assert_eq!(
            split_bodies(SRC, &edited, &ranges()),
            Err(Rejection::Unbalanced { fqn: "add".into() })
        );
    }

    #[test]
    fn braces_in_strings_chars_and_comments_do_not_count() {
        assert!(braces_balanced("{ \"}\" }"));
        assert!(braces_balanced("{ '{' }"));
        assert!(braces_balanced("{ r#\"}}\"# }"));
        assert!(braces_balanced("{ // }\n }"));
        assert!(braces_balanced("{ /* { /* { */ */ }"));
        assert!(braces_balanced("{ let x: &'a str = \"\"; }"));
        assert!(!braces_balanced("{ } }"));
        assert!(!braces_balanced("{ {"));
        assert!(!braces_balanced("x"));
    }

    #[test]
    fn stale_ranges_are_reported_not_applied() {
        let mut r = ranges();
        r[0].start += 1;
        assert!(matches!(
            split_bodies(SRC, SRC, &r),
            Err(Rejection::BadRanges(_))
        ));
    }

    #[test]
    fn the_id_ignores_edit_order() {
        let e = |site: u32, body: &str| BodyEdit {
            site_id: site,
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: "f".into(),
            body: body.into(),
        };
        assert_eq!(
            mutant_id(&[e(1, "{ 1 }"), e(2, "{ 2 }")]),
            mutant_id(&[e(2, "{ 2 }"), e(1, "{ 1 }")])
        );
        assert_ne!(mutant_id(&[e(1, "{ 1 }")]), mutant_id(&[e(1, "{ 2 }")]));
    }
}
