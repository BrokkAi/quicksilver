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

/// [`split_bodies`] for ranges that may nest (a function inside another's
/// body). The outermost bodies are split first; a changed outer body is then
/// split against the functions inside it. When only inner bodies changed,
/// the edits are theirs; when the outer function's own text changed, the
/// whole outer body is the edit.
pub fn split_nested(
    original: &str,
    edited: &str,
    bodies: &[BodyRange],
) -> Result<Vec<(u32, String)>, Rejection> {
    let mut sorted: Vec<&BodyRange> = bodies.iter().collect();
    sorted.sort_by_key(|b| (b.start, std::cmp::Reverse(b.end)));
    let mut tops: Vec<BodyRange> = Vec::new();
    for b in sorted {
        if tops
            .last()
            .is_some_and(|t| b.start >= t.start && b.end <= t.end)
        {
            continue;
        }
        tops.push(b.clone());
    }
    let mut out = Vec::new();
    for (site_id, new_body) in split_bodies(original, edited, &tops)? {
        let top = tops
            .iter()
            .find(|t| t.site_id == site_id)
            .expect("split returns its own sites");
        let inner: Vec<BodyRange> = bodies
            .iter()
            .filter(|b| b.start > top.start && b.end < top.end)
            .map(|b| BodyRange {
                site_id: b.site_id,
                fqn: b.fqn.clone(),
                start: b.start - top.start,
                end: b.end - top.start,
            })
            .collect();
        if !inner.is_empty() {
            if let Ok(sub) = split_nested(&original[top.start..top.end], &new_body, &inner) {
                if !sub.is_empty() {
                    out.extend(sub);
                    continue;
                }
            }
        }
        out.push((site_id, new_body));
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
    /// The function's first line at the map's commit.
    #[serde(default)]
    pub line: usize,
    /// The body at the map's commit, braces included.
    #[serde(default)]
    pub original: String,
    /// The new body, braces included.
    pub body: String,
}

/// A mutant captured from the checkout: every changed body, switched on
/// together.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditedMutant {
    pub id: String,
    pub edits: Vec<BodyEdit>,
    /// What the author meant the mutant to model, from `build --intents`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
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

/// How lenient a capture is.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureOptions {
    /// Drop edits outside function bodies (and added, removed or
    /// untracked-function files) with a warning, instead of rejecting the
    /// whole round.
    pub skip_outside_bodies: bool,
}

/// What one capture found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// One mutant per edited function.
    pub mutants: Vec<EditedMutant>,
    /// Problems that reject the round; when non-empty, `mutants` is empty.
    pub problems: Vec<String>,
    /// Edits dropped under `skip_outside_bodies`, one message each.
    pub dropped: Vec<String>,
}

/// Compare the checkout with the files the map was built from and lift the
/// changed bodies out. Original text comes from the map's commit. Each
/// edited function becomes its own mutant, so one round of edits and one
/// build cover many mutants.
pub fn capture_checkout(
    map: &SiteMap,
    checkout: &Path,
    opts: CaptureOptions,
) -> Result<CaptureOutcome> {
    let head = crate::worktree::head(checkout)?;
    if head != map.commit {
        return Ok(rejected(vec![format!(
            "HEAD is {} but the map was built at {}; run `cargo quicksilver map` again",
            &head[..12.min(head.len())],
            &map.commit[..12.min(map.commit.len())]
        )]));
    }
    let now = tracked_file_hashes(checkout)?;
    let mut problems = Vec::new();
    let mut files = Vec::new();
    for (rel, hash) in &now {
        match map.files.get(rel) {
            Some(was) if was == hash => {}
            Some(_) => {
                let edited = std::fs::read_to_string(checkout.join(rel))
                    .with_context(|| format!("read checkout {rel}"))?;
                files.push((rel.clone(), edited));
            }
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
    capture_files(map, checkout, files, problems, opts)
}

/// Capture the edits of a patch against the map's commit, with no checkout
/// to apply it to or reset. Paths in the patch are relative to the
/// repository's top level, as `git diff` writes them (`a/` and `b/`
/// prefixes are stripped).
pub fn capture_patch(
    map: &SiteMap,
    repo_dir: &Path,
    patch: &Path,
    opts: CaptureOptions,
) -> Result<CaptureOutcome> {
    let text = std::fs::read_to_string(patch)
        .with_context(|| format!("read patch {}", patch.display()))?;
    let mut problems = Vec::new();
    let mut touched = Vec::new();
    for f in patch_files(&text) {
        match (&f.old, &f.new) {
            (None, Some(new)) => problems.push(format!(
                "`{new}` is added by the patch; a mutant may only change function bodies"
            )),
            (Some(old), None) => problems.push(format!(
                "`{old}` is removed by the patch; a mutant may only change function bodies"
            )),
            (Some(old), Some(new)) if old != new => problems.push(format!(
                "`{old}` is renamed to `{new}` by the patch; a mutant may only change function bodies"
            )),
            (Some(old), Some(_)) => touched.push(old.clone()),
            (None, None) => {}
        }
    }
    if touched.is_empty() && problems.is_empty() {
        return Ok(rejected(vec![format!(
            "{} changes no file",
            patch.display()
        )]));
    }
    // Apply the patch in a scratch directory holding the original files, so
    // nothing in the repository changes. The ceiling keeps git from treating
    // the directory as part of an enclosing repository.
    let scratch = std::env::temp_dir().join(format!(
        "qs-patch-{}-{}",
        std::process::id(),
        crate::sitemap::random_id()
    ));
    std::fs::create_dir_all(&scratch)?;
    let result = (|| -> Result<CaptureOutcome> {
        for top in &touched {
            let dest = scratch.join(top);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, crate::worktree::show(repo_dir, &map.commit, top)?)?;
        }
        let patch_abs = patch.canonicalize().unwrap_or_else(|_| patch.to_path_buf());
        let out = std::process::Command::new("git")
            .args(["apply", "--whitespace=nowarn", "-p1"])
            .arg(&patch_abs)
            .current_dir(&scratch)
            .env(
                "GIT_CEILING_DIRECTORIES",
                scratch.parent().unwrap_or(&scratch),
            )
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .context("git apply")?;
        if !out.status.success() {
            return Ok(rejected(vec![format!(
                "{} does not apply at the map's commit {}: {}",
                patch.display(),
                &map.commit[..12.min(map.commit.len())],
                String::from_utf8_lossy(&out.stderr).trim()
            )]));
        }
        let mut problems = problems.clone();
        let mut files = Vec::new();
        for top in &touched {
            let Some(rel) = top.strip_prefix(map.prefix.as_str()) else {
                problems.push(format!(
                    "`{top}` is outside the mapped workspace `{}`",
                    map.prefix
                ));
                continue;
            };
            let edited = std::fs::read_to_string(scratch.join(top))
                .with_context(|| format!("read patched {top}"))?;
            files.push((rel.to_string(), edited));
        }
        capture_files(map, repo_dir, files, problems, opts)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn rejected(problems: Vec<String>) -> CaptureOutcome {
    CaptureOutcome {
        problems,
        ..CaptureOutcome::default()
    }
}

/// A file a patch touches: its old and new paths, `None` for /dev/null.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchFile {
    old: Option<String>,
    new: Option<String>,
}

/// The files a unified diff touches, from its `---`/`+++` headers.
fn patch_files(text: &str) -> Vec<PatchFile> {
    let path = |raw: &str| -> Option<String> {
        let raw = raw.split('\t').next().unwrap_or(raw).trim();
        if raw == "/dev/null" {
            return None;
        }
        Some(
            raw.strip_prefix("a/")
                .or_else(|| raw.strip_prefix("b/"))
                .unwrap_or(raw)
                .to_string(),
        )
    };
    let mut out = Vec::new();
    let mut old: Option<Option<String>> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            old = Some(path(rest));
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(old) = old.take() {
                out.push(PatchFile {
                    old,
                    new: path(rest),
                });
            }
        }
    }
    out
}

/// Capture edited file contents (workspace-relative path, edited text).
/// `problems` carries what the caller already found; under
/// `skip_outside_bodies` they become dropped edits instead.
fn capture_files(
    map: &SiteMap,
    git_dir: &Path,
    files: Vec<(String, String)>,
    mut problems: Vec<String>,
    opts: CaptureOptions,
) -> Result<CaptureOutcome> {
    let mut dropped = Vec::new();
    if opts.skip_outside_bodies {
        dropped.append(&mut problems);
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
    for (rel, edited) in files {
        let Some(ranges) = by_file.get(rel.as_str()) else {
            let without_site: Vec<&str> = map
                .dropped
                .iter()
                .filter(|(path, _, _)| *path == rel)
                .map(|(_, fqn, _)| fqn.as_str())
                .collect();
            let why = if without_site.is_empty() {
                format!(
                    "`{rel}` changed; it has no mutable function (test code and files without function bodies are never mutated)"
                )
            } else {
                format!(
                    "`{rel}` changed; its functions have no mutation site ({}); see `dropped` in the map for why",
                    without_site.join(", ")
                )
            };
            if opts.skip_outside_bodies {
                dropped.push(why);
            } else {
                problems.push(why);
            }
            continue;
        };
        let original = String::from_utf8(crate::worktree::show(
            git_dir,
            &map.commit,
            &format!("{}{rel}", map.prefix),
        )?)
        .with_context(|| format!("{rel} at {} is not UTF-8", map.commit))?;
        let bodies = match split_nested(&original, &edited, ranges) {
            Ok(bodies) => bodies,
            Err(Rejection::BadRanges(why)) => {
                problems.push(format!("`{rel}`: {}", Rejection::BadRanges(why)));
                continue;
            }
            Err(why) if !opts.skip_outside_bodies => {
                problems.push(format!("`{rel}`: {why}"));
                continue;
            }
            Err(_) => {
                let (kept, lines_dropped) = keep_body_hunks(&original, &edited, ranges);
                dropped.push(format!(
                    "`{rel}`: dropped the edits outside function bodies at line(s) {} of the edited file",
                    lines_dropped
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                match split_nested(&original, &kept, ranges) {
                    Ok(bodies) => bodies,
                    Err(why) => {
                        problems.push(format!("`{rel}`: {why}"));
                        continue;
                    }
                }
            }
        };
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
                line: site.start_line,
                original: original
                    .get(site.body_start_byte..site.body_end_byte)
                    .unwrap_or_default()
                    .to_string(),
                body,
            });
        }
    }
    if !problems.is_empty() {
        return Ok(CaptureOutcome {
            mutants: Vec::new(),
            problems,
            dropped,
        });
    }
    edits.sort_by_key(|e| (e.path.clone(), e.site_id));
    let mutants = edits
        .into_iter()
        .map(|e| {
            let one = vec![e];
            EditedMutant {
                id: mutant_id(&one),
                edits: one,
                intent: None,
            }
        })
        .collect();
    Ok(CaptureOutcome {
        mutants,
        problems,
        dropped,
    })
}

/// Attach author's notes to mutants. A key is a function's full name as QS
/// prints it, or `path:line` with any line of the function (the path
/// relative to the workspace or, with `prefix`, to the repository). A key
/// that matches several mutants is not applied: guessing is how notes ended
/// up on sibling mutants. `spans` gives each mutant's first and last line.
///
/// One intents file may cover several builds, so a key that matches no
/// mutant of this build is only reported when it names none of `functions`
/// (every function in the map, as full name, path and line span): a typo or
/// a name from another commit. The rest are counted in one line.
/// Returns the lines to print.
pub fn attach_intents(
    mutants: &mut [EditedMutant],
    spans: &[(usize, usize)],
    intents: &BTreeMap<String, String>,
    prefix: &str,
    functions: &[(&str, &str, (usize, usize))],
) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut elsewhere = 0usize;
    for (key, text) in intents {
        let by_line = key.rsplit_once(':').and_then(|(path, line)| {
            let line: usize = line.parse().ok()?;
            Some((path.strip_prefix(prefix).unwrap_or(path), line))
        });
        let names = |fqn: &str, path: &str, span: (usize, usize)| {
            fqn == key
                || by_line.is_some_and(|(p, line)| path == p && span.0 <= line && line <= span.1)
        };
        let hits: Vec<usize> = mutants
            .iter()
            .enumerate()
            .filter(|(i, m)| m.edits.iter().any(|e| names(&e.fqn, &e.path, spans[*i])))
            .map(|(i, _)| i)
            .collect();
        match hits.as_slice() {
            [] if functions
                .iter()
                .any(|(fqn, path, span)| names(fqn, path, *span)) =>
            {
                elsewhere += 1
            }
            [] => warnings.push(format!(
                "intent for `{key}` names no function in the map (a typo, or a name from another commit?)"
            )),
            [one] => mutants[*one].intent = Some(text.clone()),
            many => warnings.push(format!(
                "intent for `{key}` matched {} mutants ({}); not applied",
                many.len(),
                many.iter()
                    .map(|i| mutants[*i].id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    if elsewhere > 0 {
        warnings.push(format!(
            "{elsewhere} intent(s) name functions that this build did not capture"
        ));
    }
    warnings
}

/// `edited` with every change outside the function bodies undone: each
/// line hunk is kept when applying it alone to `original` still splits into
/// bodies, so a hunk survives exactly when `build` would accept it by
/// itself. Returns the result and the first edited line of each dropped
/// hunk (1-based).
pub fn keep_body_hunks(original: &str, edited: &str, ranges: &[BodyRange]) -> (String, Vec<usize>) {
    let hunks = crate::linediff::diff(
        &crate::linediff::lines(original),
        &crate::linediff::lines(edited),
    );
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for i in 0..hunks.len() {
        let alone = crate::linediff::apply(original, edited, &hunks, &[i]);
        if split_nested(original, &alone, ranges).is_ok() {
            kept.push(i);
        } else {
            dropped.push(hunks[i].b.start + 1);
        }
    }
    (
        crate::linediff::apply(original, edited, &hunks, &kept),
        dropped,
    )
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
    fn an_edit_belongs_to_the_innermost_function_it_changes() {
        let src =
            "fn outer() -> u32 {\n    fn inner() -> u32 {\n        1\n    }\n    inner() + 1\n}\n";
        let range = |id: u32, fqn: &str, open: &str, close: &str| {
            let start = src.find(open).unwrap() + open.len() - 1;
            let end = src[start..].find(close).unwrap() + start + close.len();
            BodyRange {
                site_id: id,
                fqn: fqn.into(),
                start,
                end,
            }
        };
        let ranges = vec![
            range(1, "outer", "fn outer() -> u32 {", "\n}"),
            range(2, "outer.inner", "fn inner() -> u32 {", "\n    }"),
        ];
        // Only the inner body changed: the inner function's edit.
        let edited = src.replace("        1\n", "        2\n");
        assert_eq!(
            split_nested(src, &edited, &ranges),
            Ok(vec![(2, "{\n        2\n    }".into())])
        );
        // The outer function's own text changed: the whole outer body.
        let edited = src.replace("inner() + 1", "inner() + 2");
        let got = split_nested(src, &edited, &ranges).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 1);
        assert!(got[0].1.contains("inner() + 2"));
        // Both changed: still one outer edit, which carries both.
        let edited = src
            .replace("        1\n", "        2\n")
            .replace("inner() + 1", "inner() + 2");
        let got = split_nested(src, &edited, &ranges).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 1);
        // Flat ranges behave as before.
        assert_eq!(split_nested(SRC, SRC, &super::tests::ranges()), Ok(vec![]));
    }

    #[test]
    fn edits_outside_bodies_are_dropped_hunk_by_hunk() {
        // A body edit in `add`, a doc comment added above `one`, and a body
        // edit in `Thing::name`.
        let edited = SRC
            .replace("    a + b\n", "    a - b\n")
            .replace("pub fn one()", "/// added\npub fn one()")
            .replace("format!(\"{}\", self.n)", "String::new()");
        assert!(split_bodies(SRC, &edited, &ranges()).is_err());
        let (kept, dropped) = keep_body_hunks(SRC, &edited, &ranges());
        assert_eq!(
            dropped,
            vec![6],
            "the doc comment's line in the edited file"
        );
        let bodies = split_bodies(SRC, &kept, &ranges()).expect("kept edits split");
        assert_eq!(
            bodies,
            vec![
                (3, "{\n    a - b\n}".into()),
                (11, "{\n        String::new()\n    }".into()),
            ]
        );
        // A one-line function whose signature and body change on one line
        // cannot be separated, so that edit goes as a whole.
        let edited = SRC.replace(
            "pub fn one() -> u32 { 1 }",
            "pub fn one(x: u8) -> u32 { 2 }",
        );
        let (kept, dropped) = keep_body_hunks(SRC, &edited, &ranges());
        assert_eq!(kept, SRC);
        assert_eq!(dropped, vec![6]);
    }

    #[test]
    fn patch_headers_name_old_and_new_files() {
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1 +1 @@\n\
-x\n\
+y\n\
--- /dev/null\n\
+++ b/src/new.rs\t2026-10-23\n\
@@ -0,0 +1 @@\n\
+z\n";
        assert_eq!(
            patch_files(patch),
            vec![
                PatchFile {
                    old: Some("src/lib.rs".into()),
                    new: Some("src/lib.rs".into())
                },
                PatchFile {
                    old: None,
                    new: Some("src/new.rs".into())
                },
            ]
        );
    }

    fn mutant_at(
        fqn: &str,
        path: &str,
        line: usize,
        lines: usize,
    ) -> (EditedMutant, (usize, usize)) {
        let m = EditedMutant {
            id: format!("id-{fqn}-{line}"),
            edits: vec![BodyEdit {
                site_id: 1,
                package: "p".into(),
                path: path.into(),
                fqn: fqn.into(),
                line,
                original: "{}".into(),
                body: "{ 1 }".into(),
            }],
            intent: None,
        };
        (m, (line, line + lines))
    }

    #[test]
    fn intents_attach_by_full_name_or_line_and_skip_ambiguous_keys() {
        let (a, a_lines) = mutant_at("k.Scope.new", "src/scope.rs", 10, 5);
        let (b, b_lines) = mutant_at("k.SchemaVersionRegistry.new", "src/registry.rs", 40, 3);
        let (c1, c1_lines) = mutant_at("k.twice", "src/t.rs", 1, 2);
        let (c2, c2_lines) = mutant_at("k.twice", "src/t.rs", 1, 2);
        let mut mutants = vec![a, b, c1, c2];
        let spans = vec![a_lines, b_lines, c1_lines, c2_lines];
        let intents: BTreeMap<String, String> = [
            ("k.Scope.new", "off-by-one at the upper bound"),
            ("src/registry.rs:42", "inverted negation"),
            ("k.twice", "ambiguous"),
            ("new", "a short name matches nothing"),
            ("k.other", "for another build"),
            ("src/other.rs:7", "also for another build"),
            ("src/other.rs:99", "past the end of every function"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        // The map also has `k.other` (lines 5 to 8 of src/other.rs), which
        // this build did not capture.
        let functions = [
            ("k.Scope.new", "src/scope.rs", a_lines),
            ("k.SchemaVersionRegistry.new", "src/registry.rs", b_lines),
            ("k.twice", "src/t.rs", c1_lines),
            ("k.other", "src/other.rs", (5, 8)),
        ];
        let warnings = attach_intents(&mut mutants, &spans, &intents, "", &functions);
        assert_eq!(
            mutants[0].intent.as_deref(),
            Some("off-by-one at the upper bound")
        );
        assert_eq!(mutants[1].intent.as_deref(), Some("inverted negation"));
        assert_eq!(mutants[2].intent, None);
        assert_eq!(mutants[3].intent, None);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings
            .iter()
            .any(|w| w.contains("k.twice") && w.contains("2 mutants")));
        assert!(warnings
            .iter()
            .any(|w| w.contains("`new`") && w.contains("names no function")));
        assert!(warnings
            .iter()
            .any(|w| w.contains("`src/other.rs:99`") && w.contains("names no function")));
        // The two keys for another build are one line, not one each.
        assert!(warnings
            .contains(&"2 intent(s) name functions that this build did not capture".to_string()));
        assert!(!warnings.iter().any(|w| w.contains("k.other")));
    }

    #[test]
    fn patch_intents_match_only_the_patch_mutants() {
        let (a, span) = mutant_at("k.same", "src/lib.rs", 1, 4);
        let mut first = vec![a.clone()];
        let mut second = vec![a];
        let functions = [
            ("k.same", "src/lib.rs", span),
            ("k.other", "src/other.rs", (1, 4)),
        ];
        for (mutants, key, note) in [
            (&mut first, "k.same", "first fault"),
            (&mut second, "src/lib.rs:3", "second fault"),
        ] {
            let intents = [
                (key.into(), note.into()),
                ("k.other".into(), "elsewhere".into()),
            ]
            .into();
            let warnings = attach_intents(mutants, &[span], &intents, "", &functions);
            assert_eq!(
                warnings,
                ["1 intent(s) name functions that this build did not capture"]
            );
        }
        assert_eq!(first[0].intent.as_deref(), Some("first fault"));
        assert_eq!(second[0].intent.as_deref(), Some("second fault"));
    }

    #[test]
    fn the_id_ignores_edit_order() {
        let e = |site: u32, body: &str| BodyEdit {
            site_id: site,
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: "f".into(),
            line: 1,
            original: "{}".into(),
            body: body.into(),
        };
        assert_eq!(
            mutant_id(&[e(1, "{ 1 }"), e(2, "{ 2 }")]),
            mutant_id(&[e(2, "{ 2 }"), e(1, "{ 1 }")])
        );
        assert_ne!(mutant_id(&[e(1, "{ 1 }")]), mutant_id(&[e(1, "{ 2 }")]));
    }
}
