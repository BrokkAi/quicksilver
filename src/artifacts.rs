//! Extend mbx's ordinary command lifetime through the first artifact copy.
//! The helper keeps its enclosing session; no target/cache policy is changed.
use anyhow::{ensure, Context, Result};
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

const FINISHED: &str = "quicksilver-cargo-finished-v1";
static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn remember_executable() -> Result<()> {
    let _ = EXECUTABLE.set(std::env::current_exe()?);
    Ok(())
}

fn executable() -> Result<PathBuf> {
    if let Some(path) = EXECUTABLE.get() {
        return Ok(path.clone());
    }
    // Library integration tests run beside the CLI Cargo built for them.
    let current = std::env::current_exe()?;
    let parent = current.parent().context("executable parent")?;
    let root = if parent.file_name().is_some_and(|n| n == "deps") {
        parent.parent().context("test executable parent")?
    } else {
        parent
    };
    let binary = root.join("cargo-quicksilver");
    ensure!(
        binary.is_file(),
        "cannot locate QS build helper at {}",
        binary.display()
    );
    Ok(binary)
}

#[derive(Debug, PartialEq)]
pub(crate) struct ArtifactCopies {
    pub dir: PathBuf,
}
impl Drop for ArtifactCopies {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
impl ArtifactCopies {
    fn new(root: &Path) -> Result<Arc<Self>> {
        let dir = root.join(format!("artifacts-{}", crate::sitemap::random_id()));
        fs::create_dir_all(&dir)?;
        Ok(Arc::new(Self { dir }))
    }

    fn copy(self: &Arc<Self>, artifacts: &mut [crate::cargo_cmd::BuiltExe]) -> Result<()> {
        for (i, artifact) in artifacts.iter_mut().enumerate() {
            let source = artifact.path.clone();
            let directory = self.dir.join("files").join(i.to_string());
            fs::create_dir_all(&directory)?;
            let dest = directory.join(source.file_name().context("artifact filename")?);
            reflink_copy::reflink_or_copy(&source, &dest).with_context(|| {
                format!(
                    "stage protected Cargo artifact {} -> {}",
                    source.display(),
                    dest.display()
                )
            })?;
            // Binary resolution uses mtime to choose among fresh fingerprints.
            // A snapshot must not replace that ordering with its copy order.
            if let Ok(time) = fs::metadata(&source).and_then(|m| m.modified()) {
                fs::File::open(&dest)?.set_modified(time)?;
            }
            artifact.path = dest;
            artifact.artifacts = Some(Arc::clone(self));
        }
        Ok(())
    }
}

/// Hidden Cargo subcommand, intentionally before leave_enclosing_session.
/// Cargo runs once inside the mbx session that owns this helper's lifetime.
pub(crate) fn helper(args: &[std::ffi::OsString], helper_bin: &Path) -> Result<i32> {
    ensure!(
        args.first().is_some_and(|a| a == "build"),
        "internal helper only accepts cargo build"
    );
    let cargo = std::env::var_os("CARGO")
        .context("Cargo did not provide its executable to the build helper")?;
    // Only Cargo's subcommand discovery needs this private directory. Keep
    // the real compiler/build-script PATH equal to the ordinary mbx session.
    let path = std::env::join_paths(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|p| p != helper_bin),
    )?;
    let status = Command::new(cargo)
        .env("PATH", path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("run protected cargo build")?;
    // Cargo has exited and flushed its output. mbx still owns the target lease.
    println!(
        "{}",
        serde_json::json!({"reason": FINISHED, "success": status.success(), "exit_code": status.code().unwrap_or(2)})
    );
    std::io::stdout().flush()?;
    let mut ack = String::new();
    std::io::stdin().read_line(&mut ack)?;
    ensure!(
        ack == "staged\n",
        "artifact staging was aborted before acknowledgment"
    );
    Ok(status.code().unwrap_or(2))
}

// In particular, unwind must close the acknowledgment pipe before scope
// joins the stderr reader. Child alone does not kill/reap on Drop.
struct RunningBuild(Option<std::process::Child>);
impl Drop for RunningBuild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            child.stdin.take();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read Cargo completion, snapshot under the still-live mbx lease, then
/// acknowledge and reap. Errors close the helper's stdin before waiting.
fn exchange<T>(
    cmd: &mut Command,
    progress: Option<&crate::progress::Progress>,
    consume: impl FnOnce(bool, &[u8]) -> Result<T>,
) -> Result<(T, Vec<u8>, Vec<u8>)> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawn protected mbx build")?;
    let err = child.stderr.take().context("protected build stderr")?;
    let out = child.stdout.take().context("protected build stdout")?;
    std::thread::scope(|scope| {
        let mut running = RunningBuild(Some(child));
        let child = running.0.as_mut().expect("live build");
        let stderr = scope.spawn(move || {
            let mut kept = Vec::new();
            for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                if let Some(progress) = progress {
                    let trimmed = line.trim_start();
                    if let Some(rest) = trimmed.strip_prefix("Compiling ") {
                        progress.set_message(rest.split_whitespace().next().unwrap_or(rest));
                        progress.inc(1);
                    } else if trimmed.starts_with("mbx[") {
                        progress.println(trimmed);
                    }
                }
                kept.extend_from_slice(line.as_bytes());
                kept.push(b'\n');
            }
            kept
        });
        let mut reader = std::io::BufReader::new(out);
        let mut stdout = Vec::new();
        let mut exit_code = None;
        let result = (|| {
            let success = loop {
                let mut line = Vec::new();
                ensure!(
                    reader.read_until(b'\n', &mut line)? != 0,
                    "mbx build helper ended before reporting Cargo completion"
                );
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                    if v.get("reason").and_then(|r| r.as_str()) == Some(FINISHED) {
                        exit_code = v
                            .get("exit_code")
                            .and_then(|v| v.as_i64())
                            .map(|n| n as i32);
                        break v
                            .get("success")
                            .and_then(|v| v.as_bool())
                            .context("missing helper build status")?;
                    }
                }
                stdout.extend_from_slice(&line);
            };
            consume(success, &stdout)
        })();
        // Release the lease on success, copy error or malformed transport.
        if let Some(mut input) = child.stdin.take() {
            if result.is_ok() {
                let _ = input.write_all(b"staged\n");
            }
        }
        let drain = reader.read_to_end(&mut stdout);
        let status = child.wait();
        let stderr = stderr.join().unwrap_or_default();
        let consumed = result.map_err(|error| {
            error.context(format!(
                "protected build failed; Cargo stdout:\n{}\nmbx/Cargo stderr:\n{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ))
        })?;
        drain.context("drain protected build stdout")?;
        let status = status.context("wait for protected mbx build")?;
        // A normal compile failure is expected during schema isolation. The
        // completion frame carries that status; transport errors are not normal.
        ensure!(
            exit_code.is_some() && status.code() == exit_code,
            "protected build helper exited unexpectedly: {status}"
        );
        Ok((consumed, stdout, stderr))
    })
}

pub(crate) fn build(
    root: &Path,
    target: &Path,
    cfg: &crate::config::Config,
    packages: &[String],
    progress: Option<&crate::progress::Progress>,
) -> Result<crate::cargo_cmd::CapturedBuild> {
    let copies = ArtifactCopies::new(&cfg.paths().runs.join("artifacts"))?;
    let bin = copies.dir.join("helper-bin");
    fs::create_dir(&bin)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(executable()?, bin.join("cargo-quicksilver"))?;
    let mut paths = vec![bin.clone()];
    let base_path = cfg
        .build_env
        .get("PATH")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    paths.extend(std::env::split_paths(&base_path));
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(root);
    crate::cargo_cmd::apply_build_env(&mut cmd, cfg);
    cmd.env("PATH", std::env::join_paths(paths)?);
    cmd.args(["quicksilver", "__cargo-build", "--helper-bin"])
        .arg(&bin);
    cmd.args(["--", "build", "--tests", "--message-format=json"]);
    cfg.cargo_features.apply(&mut cmd);
    crate::cargo_cmd::apply_package_select(&mut cmd, packages);
    crate::scratch::check_free_space(&copies.dir, cfg.max_scratch)?;
    let start = std::time::Instant::now();
    let result = exchange(&mut cmd, progress, |success, stdout| {
        let mut executables = crate::cargo_cmd::parse_all_executables(stdout, target);
        // Failed attribution rounds don't publish or stage binaries.
        if success {
            copies.copy(&mut executables)?;
        }
        Ok((success, executables))
    });
    let ((success, executables), stdout, stderr) = match result {
        Ok(result) => result,
        Err(error) => {
            let path = cfg.paths().index.join("last-staging-failure.json");
            let diagnostic = serde_json::json!({"worker": root, "target": target,
                "target_exists": target.exists(), "error": format!("{error:#}")});
            if let Err(save) =
                crate::util::write_atomic(&path, serde_json::to_vec_pretty(&diagnostic)?)
            {
                eprintln!(
                    "could not save staging diagnostic {}: {save:#}",
                    path.display()
                );
            }
            return Err(error).context(format!("artifact failure diagnostic: {}", path.display()));
        }
    };
    Ok(crate::cargo_cmd::CapturedBuild {
        success,
        executables,
        stdout,
        stderr,
        secs: start.elapsed().as_secs_f64(),
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn wrapped(lease: &Path, target: &Path) -> Command {
        let mut cmd = Command::new("sh");
        cmd.env("LEASE", lease).env("TARGET", target).arg("-c").arg(
            r#"
exec 9>"$LEASE"
flock -s 9 || exit 91
# The simulated Cargo child is gone before the completion frame is sent.
sh -c 'printf binary > "$TARGET/tool-0123456789abcdef"' || exit 92
printf '%s\n' '{"reason":"compiler-artifact","profile":{"test":true}}'
printf '%s\n' '{"reason":"quicksilver-cargo-finished-v1","success":true,"exit_code":0}'
read acknowledgment || exit 2
test "$acknowledgment" = staged || exit 3
"#,
        );
        cmd
    }

    fn lock_file(path: &Path) -> fs::File {
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    }

    #[test]
    fn cargo_exit_does_not_release_protection_until_artifacts_are_copied() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let lease = tmp.path().join("lease");
        let collector = lock_file(&lease);
        let copies = ArtifactCopies::new(tmp.path()).unwrap();
        let directory = copies.dir.clone();
        let (mut exes, _, _) = exchange(&mut wrapped(&lease, &target), None, |success, _| {
            assert!(success);
            assert!(
                collector.try_lock().is_err(),
                "collector must not enter between Cargo exit and snapshot"
            );
            let mut exes = vec![crate::cargo_cmd::BuiltExe {
                test_harness: true,
                binary_target: None,
                original_path: None,
                path: target.join("tool-0123456789abcdef"),
                manifest_dir: None,
                artifacts: None,
            }];
            copies.copy(&mut exes)?;
            assert!(
                collector.try_lock().is_err(),
                "copy completion is acknowledged only afterward"
            );
            Ok(exes)
        })
        .unwrap();
        collector.try_lock().unwrap();
        fs::remove_dir_all(&target).unwrap();
        assert_eq!(fs::read(&exes[0].path).unwrap(), b"binary");
        let duplicate = exes[0].clone();
        exes.clear();
        drop(copies);
        assert!(directory.exists(), "last BuiltExe clone owns the snapshot");
        drop(duplicate);
        assert!(!directory.exists());
    }

    #[test]
    fn snapshots_keep_fingerprint_order_and_reject_unprotected_fallbacks() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        let newer = target.join("tool-1111111111111111");
        let older = target.join("tool-2222222222222222");
        fs::write(&newer, "newer").unwrap();
        fs::write(&older, "older").unwrap();
        for (file, seconds) in [(&newer, 200), (&older, 100)] {
            fs::File::open(file)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .unwrap();
        }
        let copies = ArtifactCopies::new(tmp.path()).unwrap();
        let mut exes = vec![
            crate::cargo_cmd::BuiltExe {
                test_harness: true,
                binary_target: None,
                original_path: None,
                path: newer,
                manifest_dir: None,
                artifacts: None,
            },
            crate::cargo_cmd::BuiltExe {
                test_harness: true,
                binary_target: None,
                original_path: None,
                path: older,
                manifest_dir: None,
                artifacts: None,
            },
        ];
        copies.copy(&mut exes).unwrap();
        let mut test = crate::types::TestInfo {
            id: "p::test".into(),
            name: "test".into(),
            package: "p".into(),
            binary_id: "p".into(),
            binary_name: "tool".into(),
            binary_rel: "debug/deps/tool-0000000000000000".into(),
            cwd_rel: "".into(),
            duration_secs: 0.1,
            cpu_secs: None,
        };
        let resolved = crate::exec::build::resolve_binary(&target, &test, &exes).unwrap();
        assert_eq!(
            fs::read_to_string(resolved).unwrap(),
            "newer",
            "copy order must not choose an older fingerprint"
        );
        let ghost = "debug/deps/ghost-0000000000000000";
        fs::write(target.join(ghost), "stale").unwrap();
        test.binary_rel = ghost.into();
        assert!(crate::exec::build::resolve_binary(&target, &test, &exes).is_err());
    }

    #[test]
    fn copy_error_releases_command_and_removes_partial_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let lease = tmp.path().join("lease");
        let collector = lock_file(&lease);
        let copies = ArtifactCopies::new(tmp.path()).unwrap();
        let directory = copies.dir.clone();
        let error = exchange(&mut wrapped(&lease, &target), None, |_, _| {
            let mut exes = vec![crate::cargo_cmd::BuiltExe {
                test_harness: true,
                binary_target: None,
                original_path: None,
                path: target.join("missing"),
                manifest_dir: None,
                artifacts: None,
            }];
            copies.copy(&mut exes)
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("missing"));
        assert!(format!("{error:#}").contains("compiler-artifact"));
        collector.try_lock().unwrap();
        drop(copies);
        assert!(!directory.exists());
    }

    #[test]
    fn missing_completion_frame_is_an_error_not_a_successful_build() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'cargo exited early\\n'"]);
        let error = exchange(&mut cmd, None, |_, _| Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("before reporting Cargo completion"));
    }
}

/// An ordinary mbx command keeps the workspace view leased while direct tests
/// and their compile-time CARGO_BIN_EXE children use its existing outputs.
pub(crate) struct TargetLease {
    running: RunningBuild,
    _directory: Arc<ArtifactCopies>,
}
impl Drop for TargetLease {
    fn drop(&mut self) {
        if let Some(mut child) = self.running.0.take() {
            if let Some(mut input) = child.stdin.take() {
                let _ = input.write_all(b"released\n");
            }
            let _ = child.wait();
        }
    }
}

pub(crate) fn hold_target_helper() -> Result<i32> {
    println!("quicksilver-target-lease-v1");
    std::io::stdout().flush()?;
    let mut release = String::new();
    std::io::stdin().read_line(&mut release)?;
    Ok(0)
}

impl TargetLease {
    pub(crate) fn if_needed(cfg: &crate::config::Config, root: &Path) -> Result<Option<Self>> {
        let meta = crate::cargo_cmd::load_metadata_with_features(root, &cfg.cargo_features)?;
        let packages = crate::cargo_cmd::test_packages(cfg, &meta);
        let needed = meta.workspace_packages().iter().any(|p| {
            (packages.is_empty() || packages.iter().any(|n| n == p.name.as_str()))
                && p.targets
                    .iter()
                    .any(|t| t.kind.iter().any(|k| k.to_string() == "bin"))
        });
        if !needed {
            return Ok(None);
        }
        let directory = ArtifactCopies::new(&cfg.paths().runs.join("target-leases"))?;
        let bin = directory.dir.join("helper-bin");
        fs::create_dir(&bin)?;
        std::os::unix::fs::symlink(executable()?, bin.join("cargo-quicksilver"))?;
        let mut path = vec![bin];
        let original = cfg
            .build_env
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        path.extend(std::env::split_paths(&original));
        let mut cmd = crate::cargo_cmd::cargo_command();
        cmd.current_dir(root);
        crate::cargo_cmd::apply_build_env(&mut cmd, cfg);
        cmd.env("PATH", std::env::join_paths(path)?);
        cmd.args(["quicksilver", "__hold-target"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut running = RunningBuild(Some(cmd.spawn().context("start mbx target lease")?));
        let output = running
            .0
            .as_mut()
            .unwrap()
            .stdout
            .take()
            .context("target lease stdout")?;
        let mut reader = std::io::BufReader::new(output);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("wait for mbx target lease")?;
        ensure!(
            line.trim() == "quicksilver-target-lease-v1",
            "mbx target lease did not start: {line}"
        );
        Ok(Some(Self {
            running,
            _directory: directory,
        }))
    }
}
