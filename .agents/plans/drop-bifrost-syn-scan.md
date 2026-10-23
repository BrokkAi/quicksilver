# Replace Bifrost with a syn module walk and remove the dependency

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


cargo-quicksilver (QS) uses the Bifrost code-analysis library for one thing: listing the functions of a workspace with their byte ranges, names and whether they are test code. Everything else (which tests enter which function, which mutants compile, which tests fail) comes from building and running the code. On clap, opening Bifrost cold costs about four minutes of a six-and-a-half-minute `cargo quicksilver map`; the build is ten seconds and the recording run thirty-six. Bifrost is also a private, pinned dependency that needs a local patch to build some features.

After this change QS lists functions itself, with `syn`, by walking the module tree from each crate root the way rustc does. `cargo quicksilver map` on clap should take about two minutes instead of six and a half, `cargo quicksilver list` and `run` lose their Bifrost open, and the crate builds from crates.io dependencies only. The static test selection experiment (`--test-selection static`, `graph-probe`), which the owner dropped on 2026-10-21, goes with it.


## Progress


- [x] (2026-10-23 15:00Z) Surveyed every Bifrost use: `src/bifrost.rs`, `src/discover.rs` (declarations, test regions, ranges, fqns, and the diff-analysis fallback for `--in-diff`), `src/engine.rs` (open for discovery and the static selector), `src/reach.rs` and `graph-probe` (feature `static-selection`), `src/scratch.rs` (keeping `.bifrost/cache` across baseline syncs), `Cargo.toml`.
- [x] (2026-10-23 15:05Z) Collected Bifrost's fully qualified name format from the clap map so ids stay stable: see Decision Log.
- [ ] Milestone 1: `src/scan.rs`, the module walk, with unit tests on inline source.
- [ ] Milestone 2: `src/discover.rs` on the walk; `--in-diff` through git only; delete `src/bifrost.rs`, `src/reach.rs`, the feature, the `Static` selection and `graph-probe`; drop the dependencies.
- [ ] Milestone 3: tests pass; the clap map again, with the function count and the time compared with the Bifrost run (1,474 functions, 6 min 25 s).


## Surprises & Discoveries


(none yet)


## Decision Log


- Decision: keep Bifrost's fully qualified name format, so mutant ids (a hash of package, path, fqn and replacement) do not change for the common cases.
  Rationale: ids are persisted in `outcomes.jsonl`, coupling evidence and the map. The format, read off the clap map of 2026-10-23: crate name, then the module path from the file (`src/lib.rs` contributes nothing, `src/error/mod.rs` contributes `error`, `src/util/str_to_bool.rs` contributes `util.str_to_bool`), then inline modules, then for methods the self type's last path segment (trait impls name the type, not the trait; trait default methods name the trait), then the function name, all joined with `.`. Examples: `clap_builder.util.str_to_bool.str_to_bool`, `clap_builder.builder.os_str.inner.Inner.from_string`, `clap_complete.aot.shells.shell.Shell.fmt`, `clap_builder.builder.value_parser.impl_prelude._infer_ValueParser_for.value_parser`. Self types that are not a path (`&T`, tuples, slices) are rendered without whitespace; Bifrost's form for those is unknown and they are rare.
  Date/Author: 2026-10-23, Claude.

- Decision: walk the module tree from the crate root (`mod x;` to `x.rs` or `x/mod.rs`, `#[path]`, inline `mod x { }`), rather than globbing `*.rs` under `src/`.
  Rationale: a file no `mod` reaches is not compiled; a site in it would be a phantom "not covered" mutant. cargo-mutants walks the tree for the same reason.
  Date/Author: 2026-10-23, Claude.

- Decision: test code is anything under `#[cfg(test)]` (on a module, impl or function), any `#[test]` function, and any file whose path has a `tests` or `benches` component. Nothing else.
  Rationale: this is what rustc excludes from a non-test build, plus the integration-test and bench directories, which `mutable_scope` already keeps out. Name heuristics (`test_*.rs`) would drop production files.
  Date/Author: 2026-10-23, Claude.

- Decision: `--in-diff` is git-only. When the diff's target is not what is checked out, QS stops with a message to check it out, instead of falling back to Bifrost's diff analysis.
  Rationale: line-based selection needs the checked-out text; cargo-mutants has the same requirement.
  Date/Author: 2026-10-23, Claude.

- Decision: remove `--test-selection static`, `graph-probe` and `src/reach.rs` rather than keep them behind a feature.
  Rationale: the owner dropped static selection on 2026-10-21; it is the only thing that needs Bifrost's usage graph, and keeping it would keep the dependency.
  Date/Author: 2026-10-23, owner (drop) and Claude (remove, not gate).


## Outcomes & Retrospective


(to be written)


## Context and Orientation


Discovery lives in `src/discover.rs`. `scan_functions` lists every function as a `FnDecl` (package, repository-relative path, fqn, byte range of the whole item, byte range of the body with braces, line range, return kind, and the `unsafe`, `const`, skip-attribute and trivial-body flags). `discover` expands those into `types::Mutant`s, one per replacement for the return kind; `cargo quicksilver map` (in `src/exec/edited.rs`) uses the `FnDecl`s directly. Today `scan_functions` asks Bifrost for the declarations, then re-parses each function's text with `syn` (`parse_fn`) to find the body and classify the return type.

`cargo_cmd::mutable_scope(meta, package)` gives each package's root source file (lib first, else the first bin), the directory that root owns, and the entry points of its other targets, which are excluded. `cargo_cmd::package_for_path` maps a path to its package.

`util::line_col_to_offset` converts proc-macro2's line and column (1-based line, 0-based column in characters) to a byte offset.


## Plan of Work


Milestone 1. Create `src/scan.rs`:

    pub struct ScannedFn {
        pub fqn: String,
        pub start_byte: usize, pub end_byte: usize,
        pub start_line: usize, pub end_line: usize,
        pub body_start_byte: usize, pub body_end_byte: usize,
        pub sig: syn::Signature, pub attrs: Vec<syn::Attribute>, pub block: syn::Block,
        pub in_test: bool,
    }
    pub struct ScannedFile { pub path: PathBuf, pub source: String, pub functions: Vec<ScannedFn> }
    pub fn scan_crate(crate_name: &str, root_file: &Path) -> Result<Vec<ScannedFile>>

`scan_crate` parses the root with `syn::parse_file`, then walks items with a module path (`Vec<String>`), a test flag and the directory that `mod name;` resolves against. `Item::Fn` yields a function; `Item::Impl` yields its `ImplItem::Fn`s with the self type's name; `Item::Trait` yields `TraitItem::Fn`s that have a default block with the trait's name; `Item::Mod` with content recurses, without content resolves the file and recurses into it. A `#[cfg(test)]` attribute anywhere on the way marks everything inside as test code, and `#[test]` marks a function. A file that cannot be found or parsed is reported with `tracing::warn!` and skipped.

A `LineIndex` (line start offsets) converts spans to byte offsets. A function's item range is `item.span()`; the body range is `block.span()`.

Unit tests on inline sources written to a temporary directory: a crate root with `mod a;` to `a.rs` and `mod b;` to `b/mod.rs` with a nested `mod c;`; an inline module; a `#[path]` module; an impl block, a trait impl and a trait with a default method; a `#[cfg(test)] mod tests` and a `#[test]` function; and the fqns these produce.

Milestone 2. In `src/discover.rs`, `scan_functions` iterates the workspace packages (`meta.workspace_packages()`), skips proc-macro crates (as today), calls `scan_crate` on each root, keeps files inside the mutable scope and outside test-like paths, drops test functions, and builds `FnDecl`s with `classify_return(&sig.output)`, `has_skip(&attrs)`, `boring_body(&block)`, `sig.unsafety`, `sig.constness`. `scan_workspace(root, filter)` no longer opens anything. `discover_in_diff` keeps `git_diff_lines` and `DiffSelection::Lines`; `DiffSelection::Symbols` and `diff_symbol_fqns` go. `discover_in` goes; `engine::prepare` calls `discover` or `discover_in_diff` on the baseline. `graph-probe`, `TestSelection::Static`, `src/reach.rs`, `src/bifrost.rs`, the `static-selection` feature and the two `brokk-bifrost-*` dependencies are removed; `scratch::sync_baseline` no longer special-cases `.bifrost/cache`. The local `[patch.crates-io]` block in `.cargo/config.toml` is removed along with the Cargo.lock it produced; the committed Cargo.lock is regenerated without the Bifrost crates.

Milestone 3. `cargo test` (the known environmental failure in `tests/clone_probe.rs` aside), then the clap map again with the same flags as the Bifrost run, recorded in Artifacts and Notes.


## Concrete Steps


From the repository root:

    cargo test --lib scan::
    cargo test
    cd /mnt/optane/quicksilver/local/clap && rm -rf /mnt/optane/quicksilver/clap-edited && \
      XDG_RUNTIME_DIR=/run/user/1000 systemd-run --user --scope -q -p MemoryMax=48G -p OOMPolicy=continue \
      /usr/bin/time -v <repo>/target/debug/cargo-quicksilver quicksilver map \
        --scratch-root /mnt/optane/quicksilver/clap-edited --clone-backend copy --allow-copy


## Validation and Acceptance


`cargo test --lib scan::` passes the module-walk tests. `cargo test --test engine` passes, including `discovery_finds_production_fns_and_skips_test_region`, `mutant_ids_stable_across_line_shift` and `in_diff_discovery_matches_full_discovery_for_changed_functions`. `cargo test --test edited` passes. `cargo tree` shows no `brokk-bifrost` crate. The clap map reports a function count close to 1,474 and finishes in well under three minutes.


## Idempotence and Recovery


All steps are plain edits and test runs; the clap map can be re-run after `rm -rf` of its scratch root. If the regenerated Cargo.lock differs beyond the removed crates, inspect it before committing.


## Artifacts and Notes


(to be filled with the clap map transcript)


## Interfaces and Dependencies


`syn` with features `full`, `parsing`, `extra-traits` and `proc-macro2` with `span-locations` are already dependencies. No new crates. `src/scan.rs` exposes `scan_crate`, `ScannedFile`, `ScannedFn`; `src/discover.rs` keeps `discover`, `discover_in_diff`, `scan_workspace`, `FnDecl`, `ReturnKind`, `mutant_id`, `disambiguate_ids`, `apply_mutant`, `restore_file`, `mutants_from_snippet`, `classify_return`, `kind_is_schema_prefiltered`.
