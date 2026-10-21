//! Static test selection from Bifrost's whole-workspace usage graph.
//!
//! One `usage_graph` build gives every caller -> callee edge. Walking it
//! backwards from a mutated function reaches every function that can call it,
//! and the test functions among those are the tests that can execute the
//! mutant. The walk over-approximates on purpose: an extra test costs time,
//! a missing one can turn a Caught mutant into a false Missed. A walk that
//! meets a symbol whose callers Bifrost did not enumerate is incomplete, and
//! the caller must fall back to a wider selection.

use crate::cargo_cmd::package_for_path;
use crate::types::{Mutant, TestInfo};
use anyhow::Result;
use brokk_bifrost_analysis::analyzer::test_paths;
use brokk_bifrost_analysis::searchtools::{usage_graph, UsageGraphParams};
use brokk_bifrost_analysis::{CodeUnitType, Language, WorkspaceAnalyzer};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone)]
struct Node {
    fqn: String,
    short_name: String,
    path: String,
    package: Option<String>,
    test_context: bool,
    is_function: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReachStats {
    pub graph_secs: f64,
    pub declarations_secs: f64,
    pub nodes: usize,
    pub edges: usize,
    pub graph_complete: bool,
    pub incomplete_reasons: Vec<String>,
    pub truncated_symbols: usize,
    /// Names Bifrost saw at reference sites but could not resolve. A node
    /// with one of these names may have callers the graph does not show.
    pub unresolved_names: usize,
    /// Nodes with inbound sites Bifrost could not prove.
    pub unproven_inbound_nodes: usize,
    /// `unresolved_names=None`: every walk is incomplete.
    pub unnamed_unresolved: bool,
    pub test_functions: usize,
    pub nextest_tests: usize,
    /// nextest tests that no test-context graph node maps to. Static
    /// selection can never pick these; they are the known blind spot.
    pub unmapped_tests: usize,
    pub unmapped_test_examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StaticSelection {
    pub tests: Vec<TestInfo>,
    /// The walk met no truncated symbol and the mutant mapped to a node.
    pub complete: bool,
    pub reached_functions: usize,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct Walk {
    tests: Vec<u32>,
    reached: usize,
    incomplete: Option<String>,
}

pub struct ReachIndex {
    nodes: Vec<Node>,
    /// (path, primary range start byte) -> node. Discovery uses the same
    /// primary range, so a mutant maps by exact position, not by name.
    by_site: HashMap<(String, usize), u32>,
    by_fqn: HashMap<String, Vec<u32>>,
    callers: Vec<Vec<u32>>,
    truncated: HashSet<u32>,
    unproven: HashSet<u32>,
    unresolved_names: HashSet<String>,
    /// Bifrost reported an unresolved reference it could not name, so any
    /// declaration may have callers the graph does not show.
    unnamed_unresolved: bool,
    tests_of_node: HashMap<u32, Vec<u32>>,
    tests: Vec<TestInfo>,
    memo: Mutex<HashMap<u32, Walk>>,
    pub stats: ReachStats,
}

impl ReachIndex {
    pub fn build(
        workspace: &WorkspaceAnalyzer,
        meta: &cargo_metadata::Metadata,
        tests: &[TestInfo],
    ) -> Result<Self> {
        let analyzer = workspace.analyzer();
        let start = Instant::now();
        let graph = usage_graph(
            analyzer,
            UsageGraphParams {
                include_tests: true,
                paths: None,
                depth: 1,
            },
        );
        let graph_secs = start.elapsed().as_secs_f64();

        let start = Instant::now();
        let mut site_of_decl: HashMap<String, (String, usize)> = HashMap::new();
        let mut test_ctx_of_decl: HashMap<String, bool> = HashMap::new();
        let mut kind_of_decl: HashMap<String, bool> = HashMap::new();
        for (unit, range) in analyzer.all_declarations_with_primary_ranges() {
            let id = unit.declaration_id().as_str().to_string();
            let rel = crate::util::rel_slash(unit.source().rel_path());
            let lang = unit
                .source()
                .rel_path()
                .extension()
                .and_then(|e| e.to_str())
                .map(Language::from_extension)
                .unwrap_or(Language::None);
            let test_ctx =
                analyzer.in_test_region(&unit) || test_paths::is_test_like_path(&rel, lang);
            if let Some(range) = range {
                site_of_decl.insert(id.clone(), (rel, range.start_byte));
            }
            test_ctx_of_decl.insert(id.clone(), test_ctx);
            kind_of_decl.insert(id, unit.kind() == CodeUnitType::Function);
        }
        let declarations_secs = start.elapsed().as_secs_f64();

        let mut package_of_path: HashMap<String, Option<String>> = HashMap::new();
        let mut nodes = Vec::with_capacity(graph.nodes.len());
        let mut index_of: HashMap<String, u32> = HashMap::with_capacity(graph.nodes.len());
        let mut by_site = HashMap::new();
        let mut by_fqn: HashMap<String, Vec<u32>> = HashMap::new();
        for n in &graph.nodes {
            let id = n.id.as_str().to_string();
            let idx = nodes.len() as u32;
            let package = package_of_path
                .entry(n.path.clone())
                .or_insert_with(|| package_for_path(meta, &n.path))
                .clone();
            if let Some(site) = site_of_decl.get(&id) {
                by_site.insert(site.clone(), idx);
            }
            by_fqn.entry(n.fqn.clone()).or_default().push(idx);
            nodes.push(Node {
                fqn: n.fqn.clone(),
                short_name: segments(&n.fqn).last().copied().unwrap_or("").to_string(),
                path: n.path.clone(),
                package,
                test_context: test_ctx_of_decl.get(&id).copied().unwrap_or(false),
                is_function: kind_of_decl
                    .get(&id)
                    .copied()
                    .unwrap_or(n.kind == "function"),
            });
            index_of.insert(id, idx);
        }
        let mut callers: Vec<Vec<u32>> = vec![Vec::new(); nodes.len()];
        for e in &graph.edges {
            let (Some(&from), Some(&to)) = (
                index_of.get(e.from_id.as_str()),
                index_of.get(e.to_id.as_str()),
            ) else {
                continue;
            };
            if from != to {
                callers[to as usize].push(from);
            }
        }
        for c in &mut callers {
            c.sort_unstable();
            c.dedup();
        }
        let truncated: HashSet<u32> = graph
            .truncated_symbols
            .iter()
            .filter_map(|t| index_of.get(t.node_id.as_str()).copied())
            .collect();

        let reasons: Vec<(String, String)> = graph
            .incomplete_reasons
            .iter()
            .map(|r| (r.code.clone(), r.message.clone()))
            .collect();
        let (unresolved_names, unnamed_unresolved) = parse_unresolved_names(&reasons);
        let unproven: HashSet<u32> = parse_unproven_ids(&reasons)
            .iter()
            .filter_map(|id| index_of.get(id).copied())
            .collect();

        let (tests_of_node, mapped) = map_tests(&nodes, tests);
        let unmapped: Vec<&TestInfo> = tests
            .iter()
            .enumerate()
            .filter(|(i, _)| !mapped.contains(&(*i as u32)))
            .map(|(_, t)| t)
            .collect();
        let stats = ReachStats {
            graph_secs,
            declarations_secs,
            nodes: nodes.len(),
            edges: graph.edges.len(),
            graph_complete: graph.complete,
            incomplete_reasons: graph
                .incomplete_reasons
                .iter()
                .map(|r| format!("{}: {}", r.code, r.message))
                .collect(),
            truncated_symbols: truncated.len(),
            unresolved_names: unresolved_names.len(),
            unproven_inbound_nodes: unproven.len(),
            unnamed_unresolved,
            test_functions: tests_of_node.len(),
            nextest_tests: tests.len(),
            unmapped_tests: unmapped.len(),
            unmapped_test_examples: unmapped.iter().take(20).map(|t| t.id.clone()).collect(),
        };
        Ok(Self {
            nodes,
            by_site,
            by_fqn,
            callers,
            truncated,
            unproven,
            unresolved_names,
            unnamed_unresolved,
            tests_of_node,
            tests: tests.to_vec(),
            memo: Mutex::new(HashMap::new()),
            stats,
        })
    }

    fn node_for(&self, m: &Mutant) -> Option<u32> {
        if let Some(&i) = self.by_site.get(&(m.path.clone(), m.start_byte)) {
            return Some(i);
        }
        let candidates: Vec<u32> = self
            .by_fqn
            .get(&m.fqn)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&i| self.nodes[i as usize].path == m.path)
                    .collect()
            })
            .unwrap_or_default();
        (candidates.len() == 1).then(|| candidates[0])
    }

    pub fn select(&self, m: &Mutant) -> StaticSelection {
        let Some(start) = self.node_for(m) else {
            return StaticSelection {
                tests: Vec::new(),
                complete: false,
                reached_functions: 0,
                fallback_reason: Some(format!(
                    "no usage-graph node for {} at {}:{}",
                    m.fqn, m.path, m.start_byte
                )),
            };
        };
        let walk = self.walk(start);
        let mut tests: Vec<TestInfo> = walk
            .tests
            .iter()
            .filter_map(|&i| self.tests.get(i as usize).cloned())
            .collect();
        tests.sort_by(|a, b| a.id.cmp(&b.id));
        StaticSelection {
            tests,
            complete: walk.incomplete.is_none(),
            reached_functions: walk.reached,
            fallback_reason: walk.incomplete,
        }
    }

    /// Why the graph may be missing callers of `n`, if it may.
    fn node_gap(&self, n: u32) -> Option<String> {
        let node = &self.nodes[n as usize];
        if self.unnamed_unresolved {
            return Some(
                "Bifrost reported unresolved references without names (unresolved_names=None)"
                    .into(),
            );
        }
        if IMPLICITLY_CALLED.contains(&node.short_name.as_str()) {
            return Some(format!(
                "{} may be called implicitly (operator, formatting, drop, deref, or derive)",
                node.fqn
            ));
        }
        if self.truncated.contains(&n) {
            return Some(format!("callers of {} were truncated by Bifrost", node.fqn));
        }
        if self.unproven.contains(&n) {
            return Some(format!("{} has unproven inbound references", node.fqn));
        }
        if self.unresolved_names.contains(&node.short_name) {
            return Some(format!(
                "`{}` is an unresolved name at some reference site ({})",
                node.short_name, node.fqn
            ));
        }
        None
    }

    fn walk(&self, start: u32) -> Walk {
        if let Some(w) = self.memo.lock().unwrap().get(&start) {
            return w.clone();
        }
        let mut seen: HashSet<u32> = HashSet::from([start]);
        let mut queue: VecDeque<u32> = VecDeque::from([start]);
        let mut tests: BTreeSet<u32> = BTreeSet::new();
        let mut incomplete: Option<String> = None;
        while let Some(n) = queue.pop_front() {
            if incomplete.is_none() {
                incomplete = self.node_gap(n);
            }
            if let Some(ts) = self.tests_of_node.get(&n) {
                tests.extend(ts.iter().copied());
            }
            for &c in &self.callers[n as usize] {
                if seen.insert(c) {
                    queue.push_back(c);
                }
            }
        }
        let walk = Walk {
            tests: tests.into_iter().collect(),
            reached: seen
                .iter()
                .filter(|&&i| self.nodes[i as usize].is_function)
                .count(),
            incomplete,
        };
        self.memo.lock().unwrap().insert(start, walk.clone());
        walk
    }
}

/// Bifrost spells Rust fqns with dots (`tiny_a.tests.add_works`); libtest
/// uses `::`. Split on both.
fn segments(s: &str) -> Vec<&str> {
    s.split("::")
        .flat_map(|x| x.split('.'))
        .filter(|x| !x.is_empty())
        .collect()
}

/// Trait methods Rust calls without a reference site the call graph can
/// see: operators, `format!`/`to_string`, `Drop`, auto-deref, `?`
/// conversions, iteration, and code generated by `#[derive]`. A walk that
/// reaches one of these cannot know all of its callers.
const IMPLICITLY_CALLED: &[&str] = &[
    "fmt",
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "lt",
    "le",
    "gt",
    "ge",
    "max",
    "min",
    "clamp",
    "hash",
    "clone",
    "clone_from",
    "drop",
    "deref",
    "deref_mut",
    "index",
    "index_mut",
    "add",
    "sub",
    "mul",
    "div",
    "rem",
    "neg",
    "not",
    "bitand",
    "bitor",
    "bitxor",
    "shl",
    "shr",
    "add_assign",
    "sub_assign",
    "mul_assign",
    "div_assign",
    "rem_assign",
    "bitand_assign",
    "bitor_assign",
    "bitxor_assign",
    "shl_assign",
    "shr_assign",
    "from",
    "into",
    "try_from",
    "try_into",
    "from_str",
    "from_iter",
    "into_iter",
    "next",
    "next_back",
    "size_hint",
    "default",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "serialize",
    "deserialize",
    "visit_str",
    "visit_borrowed_str",
    "visit_string",
    "expecting",
    "poll",
    "call",
    "call_mut",
    "call_once",
    "extend",
    "write_str",
    "write_char",
    "write_fmt",
    "to_string",
    "branch",
    "from_output",
    "from_residual",
];

/// `unresolved_names=Some({"a", "b"})` inside a
/// `selected_reference_graph_incomplete` message. Bifrost reports these as
/// text only. `unresolved_names=None` means some unresolved reference had
/// no readable name; Bifrost's contract is to abstain on the whole pass,
/// which the second value reports.
fn parse_unresolved_names(reasons: &[(String, String)]) -> (HashSet<String>, bool) {
    let mut out = HashSet::new();
    let mut unnamed = false;
    for (code, message) in reasons {
        if code != "selected_reference_graph_incomplete" {
            continue;
        }
        if message.contains("unresolved_names=None") {
            unnamed = true;
            continue;
        }
        let Some(start) = message.find("unresolved_names=Some({") else {
            continue;
        };
        let rest = &message[start + "unresolved_names=Some({".len()..];
        let Some(end) = rest.find("})") else { continue };
        for name in rest[..end].split(", ") {
            let name = name.trim().trim_matches('"');
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    (out, unnamed)
}

/// Declaration ids in `unproven_reference_edges` messages.
fn parse_unproven_ids(reasons: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (code, message) in reasons {
        if code != "unproven_reference_edges" {
            continue;
        }
        let mut rest = message.as_str();
        while let Some(i) = rest.find("decl:v1:") {
            let tail = &rest[i..];
            let len = "decl:v1:".len()
                + tail["decl:v1:".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .count();
            out.push(tail[..len].to_string());
            rest = &tail[len..];
        }
    }
    out
}

/// Map test-context function nodes to nextest tests.
///
/// A libtest name is the module path inside the test binary's crate
/// (`tests::add_works`); a Bifrost fqn may carry the crate name in front.
/// A node matches a test when the test name, cut at some segment equal to
/// the node's last segment, is a segment suffix of the node's fqn. The cut
/// lets a macro-generated child (`tests::case::case_1`) map to its
/// attributed function (`tests::case`). Package must agree. Ambiguity maps
/// to every candidate: over-selection is safe.
fn map_tests(nodes: &[Node], tests: &[TestInfo]) -> (HashMap<u32, Vec<u32>>, HashSet<u32>) {
    let mut by_segment: HashMap<&str, Vec<(u32, usize)>> = HashMap::new();
    let test_segs: Vec<Vec<&str>> = tests.iter().map(|t| segments(&t.name)).collect();
    for (ti, segs) in test_segs.iter().enumerate() {
        for (pos, seg) in segs.iter().enumerate() {
            by_segment.entry(seg).or_default().push((ti as u32, pos));
        }
    }
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut mapped = HashSet::new();
    for (ni, node) in nodes.iter().enumerate() {
        if !node.test_context || !node.is_function {
            continue;
        }
        let fqn = segments(&node.fqn);
        let Some(last) = fqn.last() else { continue };
        let Some(cands) = by_segment.get(last) else {
            continue;
        };
        for &(ti, pos) in cands {
            let t = &tests[ti as usize];
            if node.package.as_deref() != Some(t.package.as_str()) {
                continue;
            }
            let prefix = &test_segs[ti as usize][..=pos];
            if prefix.len() <= fqn.len() && fqn[fqn.len() - prefix.len()..] == *prefix {
                out.entry(ni as u32).or_default().push(ti);
                mapped.insert(ti);
            }
        }
    }
    for v in out.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    (out, mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(fqn: &str, pkg: &str) -> Node {
        Node {
            fqn: fqn.into(),
            short_name: segments(fqn).last().unwrap().to_string(),
            path: "x.rs".into(),
            package: Some(pkg.into()),
            test_context: true,
            is_function: true,
        }
    }

    fn test(name: &str, pkg: &str) -> TestInfo {
        TestInfo {
            id: format!("{pkg}::{name}"),
            name: name.into(),
            package: pkg.into(),
            binary_id: pkg.into(),
            binary_name: pkg.into(),
            binary_rel: String::new(),
            cwd_rel: String::new(),
            duration_secs: 0.1,
        }
    }

    #[test]
    fn maps_by_segment_suffix_and_package() {
        let nodes = vec![
            node("tiny_a.tests.add_works", "tiny_a"),
            node("tiny_b::tests::add_works", "tiny_b"),
            node("tiny_a.tests.rstest_case", "tiny_a"),
        ];
        let tests = vec![
            test("tests::add_works", "tiny_a"),
            test("tests::add_works", "tiny_b"),
            test("tests::rstest_case::case_1", "tiny_a"),
            test("tests::orphan", "tiny_a"),
        ];
        let (map, mapped) = map_tests(&nodes, &tests);
        assert_eq!(map.get(&0), Some(&vec![0]));
        assert_eq!(map.get(&1), Some(&vec![1]));
        assert_eq!(map.get(&2), Some(&vec![2]));
        assert!(!mapped.contains(&3));
    }

    #[test]
    fn parses_text_only_incompleteness_signals() {
        let reasons = vec![
            (
                "selected_reference_graph_incomplete".to_string(),
                r#"rust: forward=Incomplete { reasons: [] }; kind_projection_complete=false; unresolved_names=Some({"Debug", "fmt", "new"})"#.to_string(),
            ),
            (
                "unproven_reference_edges".to_string(),
                r#"rust: unproven inbound sites [(DeclarationId("decl:v1:00ff"), 2), (DeclarationId("decl:v1:abc1"), 1)]"#.to_string(),
            ),
        ];
        let (names, unnamed) = parse_unresolved_names(&reasons);
        assert!(names.contains("fmt") && names.contains("new") && names.len() == 3);
        assert!(!unnamed);
        let none = vec![(
            "selected_reference_graph_incomplete".to_string(),
            "rust: forward=Incomplete { reasons: [] }; kind_projection_complete=false; unresolved_names=None".to_string(),
        )];
        assert!(parse_unresolved_names(&none).1);
        assert_eq!(
            parse_unproven_ids(&reasons),
            vec!["decl:v1:00ff".to_string(), "decl:v1:abc1".to_string()]
        );
    }
}
