use hapir_shared::schemas::StartedBy;

use super::session_base::SessionMode;

/// The exit reason from a local launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalLaunchExitReason {
    Switch,
    Exit,
}

/// Context for determining the local launch exit reason.
pub struct LocalLaunchContext {
    pub started_by: Option<StartedBy>,
    pub starting_mode: Option<SessionMode>,
}

/// Determine whether to switch to remote or exit after a local launch completes.
///
/// If started by the runner or starting in remote mode, we switch.
/// Otherwise, we exit.
pub fn get_local_launch_exit_reason(context: &LocalLaunchContext) -> LocalLaunchExitReason {
    if context.started_by == Some(StartedBy::Runner)
        || context.starting_mode == Some(SessionMode::Remote)
    {
        return LocalLaunchExitReason::Switch;
    }

    LocalLaunchExitReason::Exit
}
