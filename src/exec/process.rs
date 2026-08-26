use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Passed,
    Failed,
    Timeout,
}

pub fn run_exact(
    binary: &Path,
    test_name: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<RunStatus> {
    let mut cmd = Command::new(binary);
    cmd.current_dir(cwd);
    cmd.arg("--exact").arg(test_name);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().with_context(|| format!("spawn {}", binary.display()))?;
    match child.wait_timeout(timeout).context("wait_timeout")? {
        Some(status) => {
            if status.success() {
                Ok(RunStatus::Passed)
            } else {
                Ok(RunStatus::Failed)
            }
        }
        None => {
            kill_tree(&child);
            let _ = child.wait();
            Ok(RunStatus::Timeout)
        }
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
