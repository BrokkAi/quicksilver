//! A small line diff (Myers' algorithm) for splitting edits into hunks and
//! for showing what a mutant changed.

use std::ops::Range;

/// One change: lines `a` of the old text become lines `b` of the new text.
/// Either range may be empty (a pure insertion or deletion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub a: Range<usize>,
    pub b: Range<usize>,
}

/// Lines with their terminators, so that joining them gives the text back.
pub fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The hunks that turn `a` into `b`, in order, with no context lines.
pub fn diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Hunk> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let (a_mid, b_mid) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let matches = myers_matches(a_mid, b_mid);
    // Turn the matched pairs into the gaps between them.
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    for (mi, mj) in matches
        .into_iter()
        .chain(std::iter::once((a_mid.len(), b_mid.len())))
    {
        if mi > i || mj > j {
            hunks.push(Hunk {
                a: pre + i..pre + mi,
                b: pre + j..pre + mj,
            });
        }
        i = mi + 1;
        j = mj + 1;
    }
    hunks
}

/// Matched index pairs of a shortest edit script, increasing in both.
fn myers_matches<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let max = n + m;
    let off = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    // trace[d] is the furthest-reaching x on each diagonal before step d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = false;
    for d in 0..=max {
        trace.push(v[(off - d - 1) as usize..=(off + d + 1) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let down = k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]);
            let mut x = if down {
                v[(off + k + 1) as usize]
            } else {
                v[(off + k - 1) as usize] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                found = true;
                break;
            }
            k += 2;
        }
        if found {
            break;
        }
    }
    // Walk back from (n, m) through the recorded steps.
    let mut out = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let snap = &trace[d as usize];
        let at = |k: isize| snap[(k + d + 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x.max(0) && y > prev_y.max(0) && (d == 0 || x > prev_x && y > prev_y) {
            if a[(x - 1) as usize] != b[(y - 1) as usize] {
                break;
            }
            out.push(((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            x = prev_x;
            y = prev_y;
        }
    }
    out.reverse();
    out
}

/// Apply only `chosen` hunks (from `diff(lines(old), lines(new))`) to `old`.
pub fn apply(old: &str, new: &str, hunks: &[Hunk], chosen: &[usize]) -> String {
    let (a, b) = (lines(old), lines(new));
    let mut out = String::with_capacity(old.len());
    let mut at = 0usize;
    for (i, h) in hunks.iter().enumerate() {
        if !chosen.contains(&i) {
            continue;
        }
        out.extend(a[at..h.a.start].iter().copied());
        out.extend(b[h.b.clone()].iter().copied());
        at = h.a.end;
    }
    out.extend(a[at..].iter().copied());
    out
}

/// `-`/`+` lines for every change from `old` to `new`, each line prefixed
/// with `indent`, with one line of context around each hunk.
pub fn render(old: &str, new: &str, indent: &str) -> String {
    let (a, b) = (lines(old), lines(new));
    let mut out = String::new();
    let mut shown_to = 0usize;
    for h in diff(&a, &b) {
        let ctx_start = h.a.start.saturating_sub(1).max(shown_to);
        for line in &a[ctx_start..h.a.start] {
            out.push_str(&format!("{indent}  {}\n", line.trim_end_matches('\n')));
        }
        for line in &a[h.a.clone()] {
            out.push_str(&format!("{indent}- {}\n", line.trim_end_matches('\n')));
        }
        for line in &b[h.b.clone()] {
            out.push_str(&format!("{indent}+ {}\n", line.trim_end_matches('\n')));
        }
        let ctx_end = (h.a.end + 1).min(a.len());
        for line in &a[h.a.end..ctx_end] {
            out.push_str(&format!("{indent}  {}\n", line.trim_end_matches('\n')));
        }
        shown_to = ctx_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Length of a longest common subsequence, by dynamic programming.
    fn lcs(a: &[u8], b: &[u8]) -> usize {
        let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in 0..a.len() {
            for j in 0..b.len() {
                t[i + 1][j + 1] = if a[i] == b[j] {
                    t[i][j] + 1
                } else {
                    t[i][j + 1].max(t[i + 1][j])
                };
            }
        }
        t[a.len()][b.len()]
    }

    /// The hunks reproduce `b` from `a`, and they are minimal: the unchanged
    /// lines form a longest common subsequence.
    fn check(a: &[u8], b: &[u8]) {
        let hunks = diff(a, b);
        let mut rebuilt = Vec::new();
        let mut at = 0;
        let mut kept = 0;
        for h in &hunks {
            assert!(h.a.start >= at, "{a:?} {b:?} {hunks:?}");
            rebuilt.extend_from_slice(&a[at..h.a.start]);
            kept += h.a.start - at;
            rebuilt.extend_from_slice(&b[h.b.clone()]);
            at = h.a.end;
        }
        rebuilt.extend_from_slice(&a[at..]);
        kept += a.len() - at;
        assert_eq!(rebuilt, b, "{a:?} -> {b:?}: {hunks:?}");
        assert_eq!(kept, lcs(a, b), "{a:?} -> {b:?}: {hunks:?}");
    }

    #[test]
    fn diff_is_correct_and_minimal_on_many_small_inputs() {
        // A small deterministic generator: every pair over a 3-letter
        // alphabet up to length 5 would be 364^2 pairs; sample them.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let la = (next() % 8) as usize;
            let lb = (next() % 8) as usize;
            let a: Vec<u8> = (0..la).map(|_| b'a' + (next() % 3) as u8).collect();
            let b: Vec<u8> = (0..lb).map(|_| b'a' + (next() % 3) as u8).collect();
            check(&a, &b);
        }
        check(b"", b"");
        check(b"abc", b"");
        check(b"", b"abc");
        check(b"abc", b"abc");
    }

    #[test]
    fn apply_takes_only_the_chosen_hunks() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nc\nd\nE\n";
        let hunks = diff(&lines(old), &lines(new));
        assert_eq!(hunks.len(), 2);
        assert_eq!(apply(old, new, &hunks, &[0]), "a\nB\nc\nd\ne\n");
        assert_eq!(apply(old, new, &hunks, &[1]), "a\nb\nc\nd\nE\n");
        assert_eq!(apply(old, new, &hunks, &[0, 1]), new);
    }

    #[test]
    fn render_shows_changed_lines_with_context() {
        let shown = render("{\n    a + b\n}", "{\n    a - b\n}", "  ");
        assert_eq!(shown, "    {\n  -     a + b\n  +     a - b\n    }\n");
    }
}
