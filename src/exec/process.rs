use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;
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
    // rely on an empty extra_env meaning "no mutant active".
    cmd.env_remove("QS_MUTANT");
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
}
