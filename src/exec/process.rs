use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Passed,
    /// libtest assertion failure (exit 101). This is the only Caught signal.
    Failed,
    /// Non-101 nonzero exit (dynamic linker 127, abort, …). Not Caught.
    Error {
        code: Option<i32>,
    },
    Timeout,
}

pub enum WaitResult {
    Exited(ExitStatus),
    Timeout,
}

/// libtest reports test failures with this code. Anything else is infrastructure.
pub const LIBTEST_FAILURE_CODE: i32 = 101;

pub fn run_command_timeout(mut cmd: Command, timeout: Duration) -> Result<WaitResult> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().context("spawn")?;
    match child.wait_timeout(timeout).context("wait_timeout")? {
        Some(status) => Ok(WaitResult::Exited(status)),
        None => {
            kill_tree(&child);
            let _ = child.wait();
            Ok(WaitResult::Timeout)
        }
    }
}

pub fn classify_exit(status: ExitStatus) -> RunStatus {
    if status.success() {
        RunStatus::Passed
    } else if status.code() == Some(LIBTEST_FAILURE_CODE) {
        RunStatus::Failed
    } else {
        // SIGKILL is OOM / our tree kill racing wait(), not a libtest fail.
        // Leave SIGABRT/SEGV as Error: the mutant crashed the process.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if status.signal() == Some(libc::SIGKILL) {
                return RunStatus::Timeout;
            }
        }
        RunStatus::Error {
            code: status.code(),
        }
    }
}

pub fn run_exact(
    binary: &Path,
    test_name: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<RunStatus> {
    run_exact_with_env(binary, test_name, cwd, timeout, &[])
}

pub fn run_exact_with_env(
    binary: &Path,
    test_name: &str,
    cwd: &Path,
    timeout: Duration,
    extra_env: &[(String, String)],
) -> Result<RunStatus> {
    let mut cmd = Command::new(binary);
    cmd.current_dir(cwd);
    cmd.arg("--exact").arg(test_name);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    // Never inherit a mutant from the parent. Null-schema and identity gates
    // rely on an empty extra_env meaning "no mutant active". Site recording
    // is likewise only on when the caller asks for it.
    cmd.env_remove("QS_MUTANT");
    cmd.env_remove("QS_RECORD_SITES");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    match run_command_timeout(cmd, timeout)
        .with_context(|| format!("spawn {}", binary.display()))?
    {
        WaitResult::Exited(status) => Ok(classify_exit(status)),
        WaitResult::Timeout => Ok(RunStatus::Timeout),
    }
}

/// Keep at most this much stderr. A panicking test prints its message and
/// backtrace last, so we keep the tail, not the head.
pub const STDERR_CAP: usize = 64 * 1024;

/// Like [`run_exact_with_env`], but stderr is piped and drained into a capped
/// buffer. Returns the wall time and the captured tail (lossy UTF-8).
///
/// The reader thread drains continuously so the child never blocks on a full
/// pipe, including while `wait_timeout` is still counting down. After the wait
/// (and the tree kill on timeout) the write end is gone, the reader sees EOF,
/// and the join returns.
pub fn run_exact_capture_with_env(
    binary: &Path,
    test_name: &str,
    cwd: &Path,
    timeout: Duration,
    extra_env: &[(String, String)],
) -> Result<(RunStatus, Duration, String)> {
    let mut cmd = Command::new(binary);
    cmd.current_dir(cwd);
    cmd.arg("--exact").arg(test_name);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    // Never inherit a mutant from the parent; an empty extra_env means
    // "no mutant active", which is what the null runs depend on.
    cmd.env_remove("QS_MUTANT");
    cmd.env_remove("QS_RECORD_SITES");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;
    let reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        // Amortized trim: only copy once per CAP bytes read.
                        if buf.len() > STDERR_CAP * 2 {
                            let drop_to = buf.len() - STDERR_CAP;
                            buf.drain(..drop_to);
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if buf.len() > STDERR_CAP {
                let drop_to = buf.len() - STDERR_CAP;
                buf.drain(..drop_to);
            }
            buf
        })
    });

    let wait = match child.wait_timeout(timeout).context("wait_timeout")? {
        Some(status) => WaitResult::Exited(status),
        None => {
            kill_tree(&child);
            let _ = child.wait();
            WaitResult::Timeout
        }
    };
    let elapsed = start.elapsed();
    let bytes = match reader {
        Some(h) => h.join().unwrap_or_default(),
        None => Vec::new(),
    };
    let status = match wait {
        WaitResult::Exited(st) => classify_exit(st),
        WaitResult::Timeout => RunStatus::Timeout,
    };
    Ok((
        status,
        elapsed,
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

fn kill_tree(child: &std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_1_is_error_not_failed() {
        let mut cmd = Command::new("/bin/false");
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        match run_command_timeout(cmd, Duration::from_secs(5)).unwrap() {
            WaitResult::Exited(st) => {
                assert_eq!(classify_exit(st), RunStatus::Error { code: Some(1) })
            }
            WaitResult::Timeout => panic!("false timed out"),
        }
    }

    #[test]
    fn exit_0_is_passed() {
        let mut cmd = Command::new("/bin/true");
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        match run_command_timeout(cmd, Duration::from_secs(5)).unwrap() {
            WaitResult::Exited(st) => assert_eq!(classify_exit(st), RunStatus::Passed),
            WaitResult::Timeout => panic!("true timed out"),
        }
    }

    #[test]
    fn extra_args_do_not_change_false() {
        let status = run_exact(
            Path::new("/bin/false"),
            "some::test",
            Path::new("/"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(status, RunStatus::Error { code: Some(1) });
    }

    #[test]
    fn capture_keeps_the_stderr_tail() {
        // `--exact <name>` is appended to the argv, so use a script that
        // ignores its arguments.
        // Unique per process: another `cargo test` may be running on this box.
        let script = &format!("/tmp/qs_capture_probe_{}.sh", std::process::id());
        std::fs::write(
            script,
            "#!/bin/sh\nfor i in $(seq 1 20000); do echo \"line $i\" >&2; done\nexit 101\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // A sibling test's fork can briefly hold this script's write fd open,
        // so the first exec can hit ETXTBSY. Retry until the fd is gone.
        let mut result = None;
        for _ in 0..50 {
            match run_exact_capture_with_env(
                Path::new(script),
                "ignored",
                Path::new("/"),
                Duration::from_secs(30),
                &[],
            ) {
                Ok(r) => {
                    result = Some(r);
                    break;
                }
                Err(e) if format!("{e:#}").contains("Text file busy") => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("spawn failed for a non-ETXTBSY reason: {e:#}"),
            }
        }
        let (status, _dur, err) = result.expect("script stayed ETXTBSY after 50 tries");
        assert_eq!(status, RunStatus::Failed);
        assert!(err.len() <= STDERR_CAP, "cap not applied: {}", err.len());
        assert!(err.trim_end().ends_with("line 20000"), "kept head not tail");
        let _ = std::fs::remove_file(script);
    }

    #[test]
    fn sigkill_is_timeout_not_error() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "kill -9 $$"]);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        match run_command_timeout(cmd, Duration::from_secs(5)).unwrap() {
            WaitResult::Exited(st) => {
                assert_eq!(classify_exit(st), RunStatus::Timeout)
            }
            WaitResult::Timeout => panic!("kill -9 should exit, not hang"),
        }
    }
}
