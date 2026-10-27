//! Leaving an mbx session that quicksilver was started in.
//!
//! Run as `cargo quicksilver` through mbx's cargo shim, quicksilver inherits
//! the shim's build session: `MBX_SOCKET`, `MBX_TARGET_DIR` and
//! `MBX_WORKSPACE_ROOT` naming the user's checkout, a compiler wrapper, and a
//! `PATH` without the shim's directory (where `mbx` itself may live). Each
//! `mbx` that quicksilver starts in its own worktrees must open its own
//! session, so quicksilver undoes the outer one before doing anything else.
//!
//! mbx records what it changed in `MBX_SESSION_RESTORE`: a JSON list of
//! `[name, original value or null]` pairs, each an `OsString` in serde's
//! encoding (`{"Unix": [bytes]}`). Restoring sets each name back to its
//! original value, or removes it when there was none, as mbx's own
//! `restored_environment` does.

use std::ffi::OsString;

const RESTORE: &str = "MBX_SESSION_RESTORE";
const SOCKET: &str = "MBX_SOCKET";
/// Set by mbx outside the restore list; mbx drops them itself on restore.
const ALSO_REMOVED: &[&str] = &[
    "MBX_LAUNCH_CAPTURE",
    "MBX_CARGO_SHIM_MODE",
    "MBX_CARGO_SHIM_PATH",
];
/// What an mbx session sets, for an mbx that does not write
/// `MBX_SESSION_RESTORE` (taken from mbx 1.28; the list depends on mbx's
/// version and configuration). `PATH` is left alone here: without the
/// restore list, its original value is unknown.
const FALLBACK: &[&str] = &[
    "CARGO",
    "CARGO_INCREMENTAL",
    "CMAKE",
    "HOST_CC",
    "HOST_CXX",
    "MBX_AR_DETERMINISM",
    "MBX_BUILD",
    "MBX_BUILD_SCRIPT_EXECUTION",
    "MBX_CACHE_LINKS",
    "MBX_CC_STORE_PATH_SPECIFIC",
    "MBX_CMAKE_COMPILERS",
    "MBX_CMAKE_PROGRAMS",
    "MBX_FORWARD_COMPILER_NOTIFICATIONS",
    "MBX_INCREMENTAL_ROOT",
    "MBX_LEARNED_INCREMENTAL",
    "MBX_MANAGED_LINKER",
    "MBX_MANAGED_TARGET_LINKERS",
    "MBX_OUT_DIR_ROOT",
    "MBX_REAL_CC",
    "MBX_REAL_CXX",
    "MBX_REAL_RUSTDOC",
    "MBX_RESTORE_HARDLINK",
    "MBX_SCHED_BUILD_ID",
    "MBX_SCHED_BUILD_SLOTS",
    "MBX_SCHED_CGROUP_ROOT",
    "MBX_SCHED_CPUS",
    "MBX_SCHED_DIR",
    "MBX_SCHED_EVENTS",
    "MBX_SCHED_MEMORY_BYTES",
    "MBX_SCHED_PRESSURE",
    "MBX_SCHED_PRIORITY",
    "MBX_SCHED_SLOTS",
    "MBX_SCHED_SLOT_BYTES",
    "MBX_SCHED_SUSPEND",
    "MBX_SESSION_EAGER_INCREMENTAL",
    "MBX_SESSION_GC_AUTO",
    "MBX_SESSION_GC_CACHE_DIR",
    "MBX_SESSION_GC_EXECUTABLE",
    "MBX_SESSION_GC_MIN_FREE",
    "MBX_SESSION_GC_TARGET_ROOT",
    "MBX_SESSION_INCREMENTAL_BUDGET",
    "MBX_SESSION_LEASE",
    "MBX_SESSION_PATH",
    "MBX_SESSION_SEED_INCREMENTAL",
    "MBX_SHARE_OUT_DIR",
    "MBX_SHARE_WORKSPACE_ROOT",
    "MBX_SOCKET",
    "MBX_STAGING_DIR",
    "MBX_TARGET_DIR",
    "MBX_VERIFY",
    "MBX_VERIFY_SAMPLE_RATE",
    "MBX_WORKSPACE_ROOT",
    "RUSTC_WRAPPER",
    "RUSTDOC",
];

/// The changes that leave an enclosing mbx session, given how to read a
/// variable: each name with the value to set, or `None` to remove it.
/// Empty when there is no session.
pub fn plan(var: impl Fn(&str) -> Option<OsString>) -> Vec<(OsString, Option<OsString>)> {
    let mut changes: Vec<(OsString, Option<OsString>)> = match var(RESTORE) {
        Some(encoded) => {
            match serde_json::from_str::<Vec<(OsString, Option<OsString>)>>(
                &encoded.to_string_lossy(),
            ) {
                Ok(entries) => entries,
                Err(err) => {
                    tracing::warn!(
                        "cannot read {RESTORE} ({err}); removing the usual mbx session variables instead"
                    );
                    fallback()
                }
            }
        }
        None if var(SOCKET).is_some_and(|s| !s.is_empty()) => fallback(),
        None => return Vec::new(),
    };
    changes.push((RESTORE.into(), None));
    changes.extend(ALSO_REMOVED.iter().map(|n| (OsString::from(n), None)));
    changes
}

fn fallback() -> Vec<(OsString, Option<OsString>)> {
    FALLBACK.iter().map(|n| (OsString::from(n), None)).collect()
}

/// Leave an enclosing mbx session, if there is one. Call this first, before
/// any thread starts: it changes this process's environment.
pub fn leave_enclosing_session() {
    let changes = plan(|name| std::env::var_os(name));
    if changes.is_empty() {
        return;
    }
    tracing::debug!(
        "leaving the enclosing mbx session ({} variables)",
        changes.len()
    );
    for (name, value) in changes {
        match value {
            Some(value) => std::env::set_var(&name, value),
            None => std::env::remove_var(&name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn no_session_changes_nothing() {
        assert!(plan(env(&[("PATH", "/usr/bin")])).is_empty());
        assert!(plan(env(&[(SOCKET, "")])).is_empty());
    }

    /// The encoding mbx writes (serde's `OsString` on Unix): PATH back to
    /// `/bin`, MBX_SOCKET removed.
    #[cfg(unix)]
    #[test]
    fn the_restore_list_is_applied_as_mbx_writes_it() {
        let encoded = r#"[[{"Unix":[80,65,84,72]},{"Unix":[47,98,105,110]}],[{"Unix":[77,66,88,95,83,79,67,75,69,84]},null]]"#;
        let changes = plan(env(&[(RESTORE, encoded), (SOCKET, "/tmp/s.sock")]));
        assert_eq!(changes[0], ("PATH".into(), Some("/bin".into())));
        assert_eq!(changes[1], ("MBX_SOCKET".into(), None));
        assert!(changes.contains(&(RESTORE.into(), None)));
        assert!(changes.contains(&("MBX_LAUNCH_CAPTURE".into(), None)));
        assert!(changes.contains(&("MBX_CARGO_SHIM_PATH".into(), None)));
    }

    #[test]
    fn without_a_restore_list_the_known_session_variables_go_and_path_stays() {
        let changes = plan(env(&[(SOCKET, "/tmp/s.sock")]));
        assert!(changes.contains(&("MBX_TARGET_DIR".into(), None)));
        assert!(changes.contains(&("RUSTC_WRAPPER".into(), None)));
        assert!(!changes.iter().any(|(n, _)| n == "PATH"));
        // A list that cannot be read falls back the same way.
        let changes = plan(env(&[(RESTORE, "not json")]));
        assert!(changes.contains(&("MBX_WORKSPACE_ROOT".into(), None)));
    }
}
