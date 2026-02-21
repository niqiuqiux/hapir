use std::time::Duration;

use anyhow::Result;

/// Re-export from persistence for convenience.
pub use crate::persistence::is_process_alive;

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
        Ok(true)
    }

    #[cfg(not(unix))]
    {
        let _ = (pid, force);
        Ok(false)
    }
}

/// Wait for a process to die, escalating to SIGKILL if SIGTERM
/// doesn't work within 2 seconds.
async fn wait_for_process_to_die(pid: u32, force: bool) {
    let max_wait = Duration::from_millis(2000);
    let poll_interval = Duration::from_millis(20);
    let mut waited = Duration::ZERO;

    while is_process_alive(pid) && waited < max_wait {
        tokio::time::sleep(poll_interval).await;
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
            tokio::time::sleep(poll_interval).await;
            waited += poll_interval;
        }
    }
}

/// Recursively collect all descendant PIDs of a process (depth-first).
/// Returns PIDs in child-first order (leaves first, root last).
#[cfg(unix)]
fn collect_process_tree(pid: u32) -> Vec<u32> {
    let mut pids = Vec::new();

    if let Ok(output) = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        for line in stdout.lines() {
            if let Ok(child_pid) = line.trim().parse::<u32>() {
                pids.extend(collect_process_tree(child_pid));
            }
        }
    }

    pids.push(pid);
    pids
}

/// Kill a process and all its descendants.
/// Signals children first, then the root, then waits for all to die.
#[cfg(unix)]
pub async fn kill_process_tree(pid: u32, force: bool) -> Result<()> {
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

    Ok(())
}

/// Stub for non-Unix platforms.
#[cfg(not(unix))]
pub async fn kill_process_tree(_pid: u32, _force: bool) -> Result<()> {
    Ok(())
}
