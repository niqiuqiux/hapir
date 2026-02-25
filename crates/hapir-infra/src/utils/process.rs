/// Re-export from persistence for convenience.
pub use crate::persistence::is_process_alive;
use anyhow::{Context, Result};
#[cfg(unix)]
use procfs::process::Process as ProcfsProcess;
#[cfg(unix)]
use std::collections::HashSet;
use std::env::current_exe;
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::time::sleep;
use tracing::debug;

/// Send a signal to kill a process. If `force` is true, sends SIGKILL;
/// otherwise sends SIGTERM and escalates to SIGKILL if the process
/// doesn't die within 2 seconds.
pub async fn kill_process(pid: u32, force: bool) -> Result<bool> {
    if pid == 0 {
        return Ok(false);
    }

    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };

        let ret = unsafe { libc::kill(pid as i32, signal) };
        if ret != 0 {
            return Ok(false);
        }

        wait_for_process_to_die(pid, force).await;
        return Ok(true);
    }

    #[cfg(windows)]
    {
        // On Windows, use taskkill. /F for force kill, /T to kill child processes.
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T"]);
        if force {
            cmd.arg("/F");
        }
        return match cmd.output() {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        };
    }

    #[cfg(not(any(unix, windows)))]
    panic!("unsupported platform");
}

/// Wait for a process to die, escalating to SIGKILL if SIGTERM
/// doesn't work within 2 seconds.
#[cfg(unix)]
async fn wait_for_process_to_die(pid: u32, force: bool) {
    let max_wait = Duration::from_millis(2000);
    let poll_interval = Duration::from_millis(20);
    let mut waited = Duration::ZERO;

    while is_process_alive(pid) && waited < max_wait {
        sleep(poll_interval).await;
        waited += poll_interval;
    }

    // Escalate to SIGKILL if SIGTERM didn't work.
    #[cfg(unix)]
    if !force && is_process_alive(pid) {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        waited = Duration::ZERO;
        let kill_wait = Duration::from_millis(1000);
        while is_process_alive(pid) && waited < kill_wait {
            sleep(poll_interval).await;
            waited += poll_interval;
        }
    }
}

/// Recursively collect all descendant PIDs of a process (depth-first).
/// Returns PIDs in child-first order (leaves first, root last).
#[cfg(unix)]
fn collect_process_tree(pid: u32) -> Vec<u32> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        result.push(p);
        if let Ok(proc) = ProcfsProcess::new(p as i32)
            && let Ok(task) = proc.task_main_thread()
            && let Ok(children) = task.children()
        {
            for child_pid in children {
                stack.push(child_pid);
            }
        }
    }
    result.reverse();
    result
}

pub async fn kill_process_tree(pid: u32, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        let pids = collect_process_tree(pid);
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };

        // Signal all processes synchronously (children first).
        for &p in &pids {
            unsafe {
                libc::kill(p as i32, signal);
            }
        }

        // Wait for each process to die.
        for &p in &pids {
            wait_for_process_to_die(p, force).await;
        }

        return Ok(());
    }

    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T"]);
        if force {
            cmd.arg("/F");
        }
        let _ = cmd.output();
        return Ok(());
    }

    #[cfg(not(any(unix, windows)))]
    panic!("unsupported platform");
}

/// Spawn the runner as a fully detached background process.
///
/// Runs `<current_exe> runner start-sync` with stdin/stdout/stderr
/// redirected to null. On Unix, creates a new session via `setsid`.
/// On Windows, uses `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` to
/// detach from the parent console.
pub fn spawn_runner_background() -> Result<()> {
    let exe = current_exe().context("failed to determine current executable path")?;
    debug!(exe = %exe.display(), "spawning runner background process");

    let mut cmd = Command::new(&exe);
    cmd.args(["runner", "start-sync"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn()
        .context("failed to spawn runner background process")?;
    Ok(())
}
