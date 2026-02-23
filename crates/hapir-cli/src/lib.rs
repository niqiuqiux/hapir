use commands::claude::ClaudeArgs;

pub mod agent;
pub mod commands;
pub mod modules;
pub mod terminal;
mod utils;

/// Default entry point — runs the claude command with the given raw args.
pub async fn run_cli(args: ClaudeArgs) -> anyhow::Result<()> {
    commands::claude::run(args).await
}
