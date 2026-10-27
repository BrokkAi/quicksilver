//! Apply captured edits to the caller's checkout and prove a command catches them.
use crate::capture::EditedMutant;
use crate::cli::{ApplyArgs, CommonOpts, ProveArgs};
use crate::config::ConfigExtras;
use crate::exec::edited::State;
use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

pub fn resolve_mutant<'a>(state: &'a State, id: &str) -> Result<&'a EditedMutant> {
    if let Some(record) = state.records.iter().find(|r| r.mutant.id == id) {
        return Ok(&record.mutant);
    }
    let matches: Vec<_> = state
        .records
        .iter()
        .filter(|r| r.mutant.id.starts_with(id))
        .collect();
    if matches.len() == 1 {
        return Ok(&matches[0].mutant);
    }
    let unknown = matches.is_empty();
    let candidates: Vec<_> = if unknown {
        state.records.iter().collect()
    } else {
        matches
    };
    bail!(
        "{} mutant id prefix {id:?}; candidates (at most 10): {}",
        if unknown { "unknown" } else { "ambiguous" },
        candidates
            .iter()
            .take(10)
            .map(|r| r.mutant.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub fn require_checkout(root: &Path, worktree_root: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("locate checkout {}", root.display()))?;
    let managed = worktree_root
        .canonicalize()
        .unwrap_or_else(|_| worktree_root.to_path_buf());
    ensure!(!root.starts_with(managed), "refusing to edit {}: it is inside QS's own worktree root {}; QS manages and resets these worktrees, so use your own checkout", root.display(), worktree_root.display());
    Ok(())
}

struct FileEdits {
    original: String,
    replacements: Vec<(Range<usize>, String)>,
}

/// Validate all bodies first, then replace from the end of each file backwards.
/// Returns a line describing each edit, including edits already in the desired state.
pub fn apply(
    root: &Path,
    worktree_root: &Path,
    mutant: &EditedMutant,
    commit: &str,
    revert: bool,
) -> Result<Vec<String>> {
    require_checkout(root, worktree_root)?;
    ensure!(
        !mutant.edits.is_empty(),
        "mutant {} has no edits",
        mutant.id
    );
    let mut files: BTreeMap<PathBuf, FileEdits> = BTreeMap::new();
    let mut messages = vec![];
    for edit in &mutant.edits {
        let path = root.join(&edit.path);
        if !files.contains_key(&path) {
            let original = std::fs::read_to_string(&path)
                .with_context(|| format!("read {} for {}", path.display(), edit.fqn))?;
            files.insert(
                path.clone(),
                FileEdits {
                    original,
                    replacements: vec![],
                },
            );
        }
        let file = files.get_mut(&path).unwrap();
        let functions = crate::locate::functions_named(&file.original, &edit.fqn)
            .with_context(|| format!("parse {}", path.display()))?;
        ensure!(
            !functions.is_empty(),
            "{}: no function named {}",
            edit.path,
            edit.fqn
        );
        let (expected, replacement) = if revert {
            (&edit.body, &edit.original)
        } else {
            (&edit.original, &edit.body)
        };
        let matching: Vec<_> = functions
            .iter()
            .filter(|f| {
                &file.original[f.body.clone()] == expected
                    || &file.original[f.body.clone()] == replacement
            })
            .collect();
        ensure!(
            matching.len() <= 1,
            "{}: several functions match {}",
            edit.path,
            edit.fqn
        );
        let Some(function) = matching.first() else {
            let current = &file.original[functions[0].body.clone()];
            bail!(
                "{}: function {} changed since the map's commit {}; refusing to {}\n{}",
                edit.path,
                edit.fqn,
                &commit[..12.min(commit.len())],
                if revert { "revert" } else { "apply" },
                crate::linediff::render(expected, current, "  ")
            );
        };
        let already = &file.original[function.body.clone()] == replacement;
        let action = match (revert, already) {
            (false, false) => "applied",
            (false, true) => "already applied",
            (true, false) => "reverted",
            (true, true) => "not applied; nothing to revert",
        };
        messages.push(format!(
            "{action} {} to {}:{} ({})",
            mutant.id, edit.path, function.line, edit.fqn
        ));
        if !already {
            file.replacements
                .push((function.body.clone(), replacement.clone()));
        }
    }
    // Overlapping edits (including nested bodies) cannot safely be combined.
    for file in files.values_mut() {
        file.replacements.sort_by_key(|(range, _)| range.start);
        ensure!(
            file.replacements
                .windows(2)
                .all(|w| w[0].0.end <= w[1].0.start),
            "mutant {} has overlapping body edits",
            mutant.id
        );
    }
    let mut written: Vec<&PathBuf> = vec![];
    for (path, file) in &files {
        if file.replacements.is_empty() {
            continue;
        }
        let mut text = file.original.clone();
        for (range, replacement) in file.replacements.iter().rev() {
            text.replace_range(range.clone(), replacement);
        }
        if let Err(error) = std::fs::write(path, text) {
            // Restore every attempted file, including a possibly truncated failed write.
            for restore in written.iter().copied().chain(std::iter::once(path)) {
                if let Err(rollback) = std::fs::write(restore, &files[restore].original) {
                    eprintln!(
                        "error: restore {} after write failure: {rollback}",
                        restore.display()
                    );
                }
            }
            return Err(error).with_context(|| format!("write {}", path.display()));
        }
        written.push(path);
    }
    Ok(messages)
}

struct RevertGuard<'a> {
    root: &'a Path,
    managed: &'a Path,
    mutant: &'a EditedMutant,
    commit: &'a str,
    armed: bool,
}
impl RevertGuard<'_> {
    fn restore(&mut self) -> Result<()> {
        let messages = apply(self.root, self.managed, self.mutant, self.commit, true)?;
        self.armed = false;
        for message in messages {
            println!("{message}");
        }
        Ok(())
    }
}
impl Drop for RevertGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.restore() {
                eprintln!(
                    "error: could not revert mutant {}: {error:#}",
                    self.mutant.id
                );
            }
        }
    }
}

fn command(root: &Path, argv: &[String]) -> Result<ExitStatus> {
    let (program, args) = argv
        .split_first()
        .context("prove requires a command after --")?;
    Command::new(program)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("start command {program:?}"))
}

/// Returns the proof outcome's process exit code and final report line.
pub fn prove(
    root: &Path,
    managed: &Path,
    mutant: &EditedMutant,
    commit: &str,
    argv: &[String],
) -> Result<(i32, String)> {
    ensure!(!argv.is_empty(), "prove requires a command after --");
    let stop = crate::engine::install_ctrlc();
    let messages = apply(root, managed, mutant, commit, false)?;
    let mut guard = RevertGuard {
        root,
        managed,
        mutant,
        commit,
        armed: true,
    };
    for message in messages {
        println!("{message}");
    }
    let with_mutant = command(root, argv);
    // Restore before propagating a spawn error; Drop also covers unwinding.
    guard.restore()?;
    let with_mutant = with_mutant?;
    // An interrupt stopped the command; its exit status proves nothing. A
    // Ctrl-C reaches the command too, and QS's handler runs on another
    // thread, so a command killed by SIGINT also counts.
    #[cfg(unix)]
    let killed_by_interrupt = {
        use std::os::unix::process::ExitStatusExt;
        with_mutant.signal() == Some(libc::SIGINT)
    };
    #[cfg(not(unix))]
    let killed_by_interrupt = false;
    ensure!(
        !stop.load(std::sync::atomic::Ordering::SeqCst) && !killed_by_interrupt,
        "interrupted; the mutant is reverted"
    );
    let without_mutant = command(root, argv)?;
    let (code, label, reason) = if !without_mutant.success() {
        (
            2,
            "inconclusive",
            "the command fails without the mutant too",
        )
    } else if with_mutant.success() {
        (1, "not caught", "the command passes with the mutant")
    } else {
        (
            0,
            "caught",
            "the command fails with the mutant and passes without it",
        )
    };
    Ok((
        code,
        format!("{label}: {} {}: {reason}", mutant.id, mutant.fqns()),
    ))
}

pub fn run_apply(common: &CommonOpts, args: ApplyArgs) -> Result<i32> {
    let (cfg, _) = crate::engine::config_from_cli(common, ConfigExtras::default())?;
    require_checkout(&cfg.repo_root, &cfg.worktree_root)?;
    let state = State::load(&cfg.paths())?;
    let mutant = resolve_mutant(&state, &args.id)?;
    for message in apply(
        &cfg.repo_root,
        &cfg.worktree_root,
        mutant,
        &state.commit,
        args.revert,
    )? {
        println!("{message}");
    }
    Ok(0)
}

pub fn run_prove(common: &CommonOpts, args: ProveArgs) -> Result<i32> {
    // All prove errors, including configuration and refused apply, use exit 3.
    let result = (|| {
        let (cfg, _) = crate::engine::config_from_cli(common, ConfigExtras::default())?;
        require_checkout(&cfg.repo_root, &cfg.worktree_root)?;
        let state = State::load(&cfg.paths())?;
        let mutant = resolve_mutant(&state, &args.id)?;
        prove(
            &cfg.repo_root,
            &cfg.worktree_root,
            mutant,
            &state.commit,
            &args.command,
        )
    })();
    match result {
        Ok((code, message)) => {
            println!("{message}");
            Ok(code)
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            Ok(3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::BodyEdit;

    fn edit(name: &str, original: &str, body: &str) -> BodyEdit {
        BodyEdit {
            site_id: 1,
            package: "pkg".into(),
            path: "lib.rs".into(),
            fqn: format!("pkg.{name}"),
            line: 1,
            original: original.into(),
            body: body.into(),
        }
    }
    fn mutant(edits: Vec<BodyEdit>) -> EditedMutant {
        EditedMutant {
            id: "aabbccdd".into(),
            edits,
            intent: None,
        }
    }
    fn sample() -> EditedMutant {
        mutant(vec![edit("answer", "{ true }", "{ false }")])
    }
    fn setup(source: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        dir
    }
    fn change(root: &Path, m: &EditedMutant, revert: bool) -> Result<Vec<String>> {
        apply(
            root,
            &root.join(".git/quicksilver"),
            m,
            "0123456789abcdef",
            revert,
        )
    }
    fn read(root: &Path) -> String {
        std::fs::read_to_string(root.join("lib.rs")).unwrap()
    }

    #[test]
    fn apply_revert_restores_exact_bytes_after_line_movement() {
        let original = "// é moved\r\n\r\nfn answer() -> bool { true }\r\n// keep this\r\n";
        let dir = setup(original);
        let messages = change(dir.path(), &sample(), false).unwrap();
        assert_eq!(messages, ["applied aabbccdd to lib.rs:3 (pkg.answer)"]);
        assert_eq!(read(dir.path()), original.replace("{ true }", "{ false }"));
        change(dir.path(), &sample(), true).unwrap();
        assert_eq!(read(dir.path()).as_bytes(), original.as_bytes());
    }

    #[test]
    fn already_applied_and_already_reverted_change_nothing() {
        let original = "fn answer() -> bool { true }\n";
        let dir = setup(original);
        let m = sample();
        assert!(change(dir.path(), &m, true).unwrap()[0].contains("not applied; nothing to revert"));
        assert_eq!(read(dir.path()), original);
        change(dir.path(), &m, false).unwrap();
        let applied = read(dir.path());
        assert!(change(dir.path(), &m, false).unwrap()[0].starts_with("already applied"));
        assert_eq!(read(dir.path()), applied);
        // Revert works without a record of how the mutant was applied.
        std::fs::write(dir.path().join("lib.rs"), original.replace("true", "false")).unwrap();
        change(dir.path(), &m, true).unwrap();
        assert_eq!(read(dir.path()), original);
    }

    #[test]
    fn changed_body_refuses_without_writing_and_shows_diff() {
        let source = "fn answer() -> bool { todo!() }\n";
        let dir = setup(source);
        let error = change(dir.path(), &sample(), false)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("changed since the map's commit 0123456789ab"),
            "{error}"
        );
        assert!(
            error.contains("- { true }") && error.contains("+ { todo!() }"),
            "{error}"
        );
        assert_eq!(read(dir.path()), source);
    }

    #[test]
    fn a_second_edit_failure_writes_nothing_in_any_file() {
        for second_file in ["lib.rs", "other.rs"] {
            let source = "fn answer() -> bool { true } fn other() { 2; }";
            let dir = setup(source);
            std::fs::write(dir.path().join("other.rs"), source).unwrap();
            let mut other = edit("other", "{ 1; }", "{}");
            other.path = second_file.into();
            let m = mutant(vec![sample().edits.remove(0), other]);
            assert!(change(dir.path(), &m, false).is_err());
            assert_eq!(read(dir.path()), source);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("other.rs")).unwrap(),
                source
            );
        }
    }

    #[test]
    fn several_edits_in_one_file_use_original_offsets() {
        let source = "fn answer() -> bool { true } fn other() { 1; }";
        let dir = setup(source);
        let m = mutant(vec![
            sample().edits.remove(0),
            edit("other", "{ 1; }", "{ let much_longer = 2; }"),
        ]);
        change(dir.path(), &m, false).unwrap();
        assert_eq!(
            read(dir.path()),
            "fn answer() -> bool { false } fn other() { let much_longer = 2; }"
        );
        change(dir.path(), &m, true).unwrap();
        assert_eq!(read(dir.path()), source);
    }

    #[test]
    fn managed_worktree_refuses_even_through_a_symlink() {
        let dir = setup("fn answer() -> bool { true }");
        let managed = dir.path().join("managed");
        let checkout = managed.join("main");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("lib.rs"), read(dir.path())).unwrap();
        let error = apply(&checkout, &managed, &sample(), "abc", false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("QS's own worktree root"), "{error}");
        assert_eq!(read(&checkout), read(dir.path()));
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(&checkout, &alias).unwrap();
            assert!(apply(&alias, &managed, &sample(), "abc", false).is_err());
        }
    }

    #[test]
    fn missing_functions_missing_files_invalid_syntax_and_duplicate_matches_refuse() {
        for source in [
            "fn another() {}",
            "fn answer(",
            "fn answer() { true } fn answer() { true }",
        ] {
            let dir = setup(source);
            assert!(change(dir.path(), &sample(), false).is_err());
            assert_eq!(read(dir.path()), source);
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(change(dir.path(), &sample(), false)
            .unwrap_err()
            .to_string()
            .contains("read"));
    }

    #[test]
    fn prove_reports_all_three_outcomes_and_restores_original() {
        let source = "fn answer() -> bool { true }\n";
        for (command, code, final_line) in [
            ("! grep -q false lib.rs", 0, "caught: aabbccdd pkg.answer: the command fails with the mutant and passes without it"),
            ("grep -q answer lib.rs", 1, "not caught: aabbccdd pkg.answer: the command passes with the mutant"),
            ("grep -q absent lib.rs", 2, "inconclusive: aabbccdd pkg.answer: the command fails without the mutant too"),
        ] {
            let dir = setup(source);
            let argv = vec!["sh".into(), "-c".into(), command.into()];
            assert_eq!(prove(dir.path(), &dir.path().join("managed"), &sample(), "abc", &argv).unwrap(), (code, final_line.into()));
            assert_eq!(read(dir.path()), source);
        }
    }

    #[test]
    fn prove_reverts_when_command_cannot_start() {
        let source = "fn answer() -> bool { true }\n";
        let dir = setup(source);
        let argv = vec![dir
            .path()
            .join("nonexistent-command")
            .to_string_lossy()
            .into_owned()];
        let error = prove(
            dir.path(),
            &dir.path().join("managed"),
            &sample(),
            "abc",
            &argv,
        )
        .unwrap_err();
        assert!(error.to_string().contains("start command"));
        assert_eq!(read(dir.path()), source);
    }

    #[test]
    fn revert_guard_restores_on_unwinding() {
        let source = "fn answer() -> bool { true }";
        let dir = setup(source);
        let m = sample();
        let managed = dir.path().join("managed");
        let result = std::panic::catch_unwind(|| {
            change(dir.path(), &m, false).unwrap();
            let _guard = RevertGuard {
                root: dir.path(),
                managed: &managed,
                mutant: &m,
                commit: "abc",
                armed: true,
            };
            panic!("command setup panicked");
        });
        assert!(result.is_err());
        assert_eq!(read(dir.path()), source);
    }

    #[test]
    fn mutant_id_prefixes_are_unique_and_errors_list_candidates() {
        let record = |id: &str| crate::exec::edited::Record {
            mutant: EditedMutant {
                id: id.into(),
                ..sample()
            },
            status: crate::exec::edited::Status::Pending,
            captured_at: String::new(),
        };
        let state = State {
            records: vec![record("aa11"), record("aa22"), record("bb33")],
            ..State::default()
        };
        assert_eq!(resolve_mutant(&state, "bb").unwrap().id, "bb33");
        assert_eq!(resolve_mutant(&state, "aa11").unwrap().id, "aa11");
        let ambiguous = resolve_mutant(&state, "aa").unwrap_err().to_string();
        assert!(
            ambiguous.contains("ambiguous") && ambiguous.contains("aa11, aa22"),
            "{ambiguous}"
        );
        let unknown = resolve_mutant(&state, "cc").unwrap_err().to_string();
        assert!(
            unknown.contains("unknown") && unknown.contains("aa11, aa22, bb33"),
            "{unknown}"
        );
    }
}
