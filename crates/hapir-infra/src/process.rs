use std::process::Stdio;

use anyhow::{Context, Result};
use tracing::debug;

/// Spawn the runner as a fully detached background process.
///
/// Runs `<current_exe> runner start-sync` with stdin/stdout/stderr
/// redirected to null and (on Unix) a new session via `setsid`.
pub fn spawn_runner_background() -> Result<()> {
    let exe = std::env::current_exe().context("failed to determine current executable path")?;
    debug!(exe = %exe.display(), "spawning runner background process");

    let mut cmd = std::process::Command::new(&exe);
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

    cmd.spawn()
        .context("failed to spawn runner background process")?;
    Ok(())
}
