//! Static test selection from Bifrost's whole-workspace usage graph.
//!
//! One `usage_graph` build gives every caller -> callee edge plus the
//! evidence for edges it could not draw (BrokkAi/bifrost-dev#6229). Walking
//! backwards from a mutated function reaches every function that can call
//! it; the test functions among those are the tests that can execute the
//! mutant. The walk over-approximates on purpose: an extra test costs time,
//! a missing one can turn a Caught mutant into a false Missed. Where the
//! graph cannot bound a function's callers, the walk is incomplete and the
//! caller falls back to every candidate test.
//!
//! The backward walk follows Bifrost's documented consumer contract:
//! 1. follow every drawn edge, proven or not;
//! 2. treat every withheld reference as a candidate edge from its caller
//!    (every declaration in its file when the caller is unknown) to every
//!    node with its terminal name, or to its named target;
//! 3. continue from the workspace trait method a node `implements`;
//! 4. continue from the self type of a node with `implicit_callers`, or
//!    give up on the walk when the self type is unknown;
//! 5. when `withheld_references_complete` is false, no walk is complete;
//! 6. a function with no graph node (a nested `fn`) maps to the node of the
//!    function that encloses it.

use crate::cargo_cmd::package_for_path;
use crate::types::{Mutant, TestInfo};
use anyhow::Result;
use brokk_bifrost_analysis::analyzer::test_paths;
use brokk_bifrost_analysis::{CodeUnitType, Language, WorkspaceAnalyzer};
use brokk_bifrost_tools::searchtools::{
    usage_graph, UsageGraphParams, UsageGraphResult, UsageGraphWithheldCallee,
};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone)]
struct Node {
    fqn: String,
    path: String,
    /// Primary range of the declaration, when Bifrost reports one.
    span: Option<(usize, usize)>,
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
    pub unproven_edges: usize,
    pub withheld_references: usize,
    pub withheld_candidate_edges: usize,
    pub withheld_references_complete: bool,
    pub implements_links: usize,
    pub implicit_caller_nodes: usize,
    pub implicit_without_self_type: usize,
    pub truncated_symbols: usize,
    pub graph_complete: bool,
    pub incomplete_reasons: Vec<String>,
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
    /// The walk bounded every caller. False means "fall back".
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

/// The backward-walk graph, independent of Bifrost's types.
#[derive(Debug, Default)]
struct Graph {
    nodes: Vec<Node>,
    /// callee -> callers, including candidate and `implements`/self-type
    /// continuations.
    callers: Vec<Vec<u32>>,
    /// Nodes whose callers cannot be bounded (truncated, or implicit
    /// callers with no self type).
    open: HashMap<u32, String>,
    /// Set when the whole graph supplies no absence evidence.
    global_gap: Option<String>,
}

impl Graph {
    fn add_caller(&mut self, callee: u32, caller: u32) {
        if callee != caller {
            self.callers[callee as usize].push(caller);
        }
    }

    fn finish(&mut self) {
        for c in &mut self.callers {
            c.sort_unstable();
            c.dedup();
        }
    }

    fn walk(&self, start: u32, tests_of_node: &HashMap<u32, Vec<u32>>) -> Walk {
        let mut seen: HashSet<u32> = HashSet::from([start]);
        let mut queue: VecDeque<u32> = VecDeque::from([start]);
        let mut tests: BTreeSet<u32> = BTreeSet::new();
        let mut incomplete = self.global_gap.clone();
        while let Some(n) = queue.pop_front() {
            if incomplete.is_none() {
                if let Some(why) = self.open.get(&n) {
                    incomplete = Some(why.clone());
                }
            }
            if let Some(ts) = tests_of_node.get(&n) {
                tests.extend(ts.iter().copied());
            }
            for &c in &self.callers[n as usize] {
                if seen.insert(c) {
                    queue.push_back(c);
                }
            }
        }
        Walk {
            tests: tests.into_iter().collect(),
            reached: seen
                .iter()
                .filter(|&&i| self.nodes[i as usize].is_function)
                .count(),
            incomplete,
        }
    }
}

pub struct ReachIndex {
    graph: Graph,
    /// nextest tests no test-context graph node maps to (macro-generated
    /// tests, for example). Static analysis cannot rule them out, so every
    /// selection that could include them does.
    unmapped: HashSet<u32>,
    /// (path, primary range start byte) -> node. Discovery uses the same
    /// primary range, so a mutant maps by exact position, not by name.
    by_site: HashMap<(String, usize), u32>,
    by_fqn: HashMap<String, Vec<u32>>,
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
        let result = usage_graph(
            analyzer,
            UsageGraphParams {
                include_same_owner: true,
                include_tests: true,
                paths: None,
                depth: 1,
            },
        );
        let graph_secs = start.elapsed().as_secs_f64();

        let start = Instant::now();
        let mut decls: HashMap<String, Decl> = HashMap::new();
        for (unit, range) in analyzer.all_declarations_with_primary_ranges() {
            let rel = crate::util::rel_slash(unit.source().rel_path());
            let lang = unit
                .source()
                .rel_path()
                .extension()
                .and_then(|e| e.to_str())
                .map(Language::from_extension)
                .unwrap_or(Language::None);
            decls.insert(
                unit.declaration_id().as_str().to_string(),
                Decl {
                    span: range.map(|r| (r.start_byte, r.end_byte)),
                    test_context: analyzer.in_test_region(&unit)
                        || test_paths::is_test_like_path(&rel, lang),
                    is_function: unit.kind() == CodeUnitType::Function,
                },
            );
        }
        let declarations_secs = start.elapsed().as_secs_f64();
        let mut index = Self::from_graph(&result, &decls, meta, tests);
        index.stats.graph_secs = graph_secs;
        index.stats.declarations_secs = declarations_secs;
        Ok(index)
    }

    fn from_graph(
        result: &UsageGraphResult,
        decls: &HashMap<String, Decl>,
        meta: &cargo_metadata::Metadata,
        tests: &[TestInfo],
    ) -> Self {
        let mut package_of_path: HashMap<String, Option<String>> = HashMap::new();
        let mut graph = Graph::default();
        let mut index_of: HashMap<&str, u32> = HashMap::with_capacity(result.nodes.len());
        let mut by_site = HashMap::new();
        let mut by_fqn: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_path: HashMap<&str, Vec<u32>> = HashMap::new();
        for n in &result.nodes {
            let idx = graph.nodes.len() as u32;
            let decl = decls.get(n.id.as_str());
            let package = package_of_path
                .entry(n.path.clone())
                .or_insert_with(|| package_for_path(meta, &n.path))
                .clone();
            if let Some((s, _)) = decl.and_then(|d| d.span) {
                by_site.insert((n.path.clone(), s), idx);
            }
            by_fqn.entry(n.fqn.clone()).or_default().push(idx);
            if let Some(name) = segments(&n.fqn).last() {
                by_name.entry((*name).to_string()).or_default().push(idx);
            }
            by_path.entry(n.path.as_str()).or_default().push(idx);
            graph.nodes.push(Node {
                fqn: n.fqn.clone(),
                path: n.path.clone(),
                span: decl.and_then(|d| d.span),
                package,
                test_context: decl.is_some_and(|d| d.test_context),
                is_function: decl.map_or(n.kind == "function", |d| d.is_function),
            });
            index_of.insert(n.id.as_str(), idx);
        }
        graph.callers = vec![Vec::new(); graph.nodes.len()];
        let mut stats = ReachStats {
            nodes: graph.nodes.len(),
            edges: result.edges.len(),
            withheld_references: result.withheld_references.len(),
            withheld_references_complete: result.withheld_references_complete,
            truncated_symbols: result.truncated_symbols.len(),
            graph_complete: result.complete,
            incomplete_reasons: result
                .incomplete_reasons
                .iter()
                .map(|r| format!("{}: {}", r.code, r.message))
                .collect(),
            ..ReachStats::default()
        };

        // 1. Every drawn edge, proven or not.
        for e in &result.edges {
            if let (Some(&from), Some(&to)) = (
                index_of.get(e.from_id.as_str()),
                index_of.get(e.to_id.as_str()),
            ) {
                graph.add_caller(to, from);
                stats.unproven_edges += usize::from(!e.proven);
            }
        }
        // 2. Withheld references as candidate edges.
        for w in &result.withheld_references {
            let callers: Vec<u32> =
                match w.from_id.as_ref().and_then(|id| index_of.get(id.as_str())) {
                    Some(&c) => vec![c],
                    None => by_path.get(w.path.as_str()).cloned().unwrap_or_default(),
                };
            let targets: Vec<u32> = match &w.callee {
                UsageGraphWithheldCallee::Named { name } => {
                    by_name.get(name.as_str()).cloned().unwrap_or_default()
                }
                UsageGraphWithheldCallee::Declaration { to_id } => index_of
                    .get(to_id.as_str())
                    .map(|&t| vec![t])
                    .unwrap_or_default(),
            };
            for &t in &targets {
                for &c in &callers {
                    graph.add_caller(t, c);
                    stats.withheld_candidate_edges += 1;
                }
            }
        }
        // 3 and 4. Trait-method and self-type continuations.
        for n in &result.nodes {
            let Some(&idx) = index_of.get(n.id.as_str()) else {
                continue;
            };
            if let Some(&t) = n
                .implements
                .as_ref()
                .and_then(|id| index_of.get(id.as_str()))
            {
                graph.add_caller(idx, t);
                stats.implements_links += 1;
            }
            if let Some(implicit) = &n.implicit_callers {
                stats.implicit_caller_nodes += 1;
                match implicit
                    .self_type_id
                    .as_ref()
                    .and_then(|id| index_of.get(id.as_str()))
                {
                    Some(&ty) => graph.add_caller(idx, ty),
                    None => {
                        stats.implicit_without_self_type += 1;
                        graph.open.insert(
                            idx,
                            format!(
                                "{} is called implicitly through `{}` and its self type is unknown",
                                n.fqn, implicit.trait_name
                            ),
                        );
                    }
                }
            }
        }
        for t in &result.truncated_symbols {
            if let Some(&idx) = index_of.get(t.node_id.as_str()) {
                graph.open.insert(
                    idx,
                    format!("callers of {} were truncated by Bifrost", t.fqn),
                );
            }
        }
        // 5. No absence evidence at all.
        if !result.withheld_references_complete {
            graph.global_gap = Some(
                "Bifrost could not name every withheld reference (withheld_references_complete=false)"
                    .into(),
            );
        }
        graph.finish();

        let (tests_of_node, mapped) = map_tests(&graph.nodes, tests);
        let unmapped: Vec<&TestInfo> = tests
            .iter()
            .enumerate()
            .filter(|(i, _)| !mapped.contains(&(*i as u32)))
            .map(|(_, t)| t)
            .collect();
        stats.test_functions = tests_of_node.len();
        stats.nextest_tests = tests.len();
        stats.unmapped_tests = unmapped.len();
        stats.unmapped_test_examples = unmapped.iter().take(20).map(|t| t.id.clone()).collect();
        let unmapped_set: HashSet<u32> = (0..tests.len() as u32)
            .filter(|i| !mapped.contains(i))
            .collect();
        Self {
            graph,
            unmapped: unmapped_set,
            by_site,
            by_fqn,
            tests_of_node,
            tests: tests.to_vec(),
            memo: Mutex::new(HashMap::new()),
            stats,
        }
    }

    /// Contract step 6. Exact position first; then a unique same-file fqn;
    /// then the smallest node in the same file whose span encloses the
    /// mutant (nested `fn` items have no graph node of their own).
    fn node_for(&self, m: &Mutant) -> Option<u32> {
        if let Some(&i) = self.by_site.get(&(m.path.clone(), m.start_byte)) {
            return Some(i);
        }
        let same_file: Vec<u32> = self
            .by_fqn
            .get(&m.fqn)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&i| self.graph.nodes[i as usize].path == m.path)
                    .collect()
            })
            .unwrap_or_default();
        if same_file.len() == 1 {
            return Some(same_file[0]);
        }
        self.graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_function && n.path == m.path)
            .filter_map(|(i, n)| n.span.map(|(s, e)| (i as u32, s, e)))
            .filter(|&(_, s, e)| s <= m.start_byte && m.end_byte <= e)
            .min_by_key(|&(_, s, e)| e - s)
            .map(|(i, _, _)| i)
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
        let cached = self.memo.lock().unwrap().get(&start).cloned();
        let walk = match cached {
            Some(w) => w,
            None => {
                let w = self.graph.walk(start, &self.tests_of_node);
                self.memo.lock().unwrap().insert(start, w.clone());
                w
            }
        };
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
}

/// `--test-selection static`: a complete walk gives the mutant's tests plus
/// the unmapped tests of its candidate packages; an incomplete walk falls
/// back to every candidate test, the same set `sites` mode starts from.
pub struct StaticSelector {
    fallback: crate::plan::PackageSelector,
    by_mutant: HashMap<String, Option<Vec<TestInfo>>>,
    pub summary: StaticSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StaticSummary {
    pub mutants: usize,
    pub complete: usize,
    pub fallback: usize,
    pub not_covered: usize,
    pub mean_selected: f64,
    pub mean_candidates: f64,
    pub graph_secs: f64,
    pub fallback_reasons: Vec<(String, usize)>,
}

impl StaticSelector {
    pub fn build(
        reach: &ReachIndex,
        mutants: &[Mutant],
        fallback: crate::plan::PackageSelector,
    ) -> Self {
        use crate::plan::TestSelector;
        let mut by_mutant = HashMap::new();
        let mut summary = StaticSummary {
            mutants: mutants.len(),
            graph_secs: reach.stats.graph_secs,
            ..StaticSummary::default()
        };
        let mut reasons: HashMap<String, usize> = HashMap::new();
        let (mut selected_total, mut candidate_total) = (0usize, 0usize);
        for m in mutants {
            let candidates = fallback.tests_for(m);
            candidate_total += candidates.len();
            let sel = reach.select(m);
            if !sel.complete {
                summary.fallback += 1;
                selected_total += candidates.len();
                let why = sel.fallback_reason.unwrap_or_default();
                let key = why.split(" (").next().unwrap_or(&why).to_string();
                *reasons.entry(key).or_default() += 1;
                by_mutant.insert(m.id.clone(), None);
                continue;
            }
            summary.complete += 1;
            let mut ids: HashSet<String> = sel.tests.iter().map(|t| t.id.clone()).collect();
            let unmapped: HashSet<&str> = reach
                .unmapped
                .iter()
                .filter_map(|&i| reach.tests.get(i as usize).map(|t| t.id.as_str()))
                .collect();
            for t in &candidates {
                if unmapped.contains(t.id.as_str()) {
                    ids.insert(t.id.clone());
                }
            }
            let chosen: Vec<TestInfo> = candidates
                .into_iter()
                .filter(|t| ids.contains(&t.id))
                .collect();
            summary.not_covered += usize::from(chosen.is_empty());
            selected_total += chosen.len();
            by_mutant.insert(m.id.clone(), Some(chosen));
        }
        let n = mutants.len().max(1) as f64;
        summary.mean_selected = selected_total as f64 / n;
        summary.mean_candidates = candidate_total as f64 / n;
        let mut reasons: Vec<(String, usize)> = reasons.into_iter().collect();
        reasons.sort_by_key(|r| std::cmp::Reverse(r.1));
        reasons.truncate(8);
        summary.fallback_reasons = reasons;
        Self {
            fallback,
            by_mutant,
            summary,
        }
    }
}

impl crate::plan::TestSelector for StaticSelector {
    fn covers(&self, m: &Mutant) -> bool {
        !self.tests_for(m).is_empty()
    }
    fn tests_for(&self, m: &Mutant) -> Vec<TestInfo> {
        match self.by_mutant.get(&m.id) {
            Some(Some(tests)) => tests.clone(),
            _ => self.fallback.tests_for(m),
        }
    }
    fn catalog(&self) -> &[TestInfo] {
        self.fallback.catalog()
    }
}

struct Decl {
    span: Option<(usize, usize)>,
    test_context: bool,
    is_function: bool,
}

/// Bifrost spells Rust fqns with dots (`tiny_a.tests.add_works`); libtest
/// uses `::`. Split on both.
fn segments(s: &str) -> Vec<&str> {
    s.split("::")
        .flat_map(|x| x.split('.'))
        .filter(|x| !x.is_empty())
        .collect()
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

    fn node(fqn: &str, pkg: &str, test: bool) -> Node {
        Node {
            fqn: fqn.into(),
            path: "x.rs".into(),
            span: None,
            package: Some(pkg.into()),
            test_context: test,
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
            node("tiny_a.tests.add_works", "tiny_a", true),
            node("tiny_b::tests::add_works", "tiny_b", true),
            node("tiny_a.tests.rstest_case", "tiny_a", true),
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

    /// 0 = mutated fn, 1 = helper calling it, 2 = test calling the helper,
    /// 3 = unrelated test. Continuations are ordinary caller edges.
    fn small_graph() -> Graph {
        let mut g = Graph {
            nodes: vec![
                node("p.f", "p", false),
                node("p.helper", "p", false),
                node("p.tests.uses_f", "p", true),
                node("p.tests.other", "p", true),
            ],
            ..Graph::default()
        };
        g.callers = vec![Vec::new(); 4];
        g.add_caller(0, 1);
        g.add_caller(1, 2);
        g.finish();
        g
    }

    #[test]
    fn walk_reaches_tests_through_callers_only() {
        let g = small_graph();
        let tests_of_node = HashMap::from([(2, vec![10]), (3, vec![11])]);
        let w = g.walk(0, &tests_of_node);
        assert_eq!(w.tests, vec![10]);
        assert!(w.incomplete.is_none());
    }

    #[test]
    fn open_node_or_global_gap_makes_the_walk_incomplete() {
        let mut g = small_graph();
        let tests_of_node = HashMap::from([(2, vec![10])]);
        g.open.insert(1, "helper is implicit".into());
        assert_eq!(
            g.walk(0, &tests_of_node).incomplete.as_deref(),
            Some("helper is implicit")
        );
        let mut g = small_graph();
        g.global_gap = Some("no absence evidence".into());
        assert!(g.walk(0, &tests_of_node).incomplete.is_some());
        assert!(g.walk(3, &tests_of_node).incomplete.is_some());
    }
}
