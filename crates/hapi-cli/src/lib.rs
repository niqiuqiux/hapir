pub mod agent;
pub mod api;
pub mod commands;
pub mod config;
pub mod handlers;
pub mod modules;
pub mod persistence;
pub mod rpc;
pub mod runner;
pub mod terminal;
pub mod utils;
pub mod ws;

/// Default entry point — runs the claude command with the given raw args.
pub async fn run_cli(args: Vec<String>) -> anyhow::Result<()> {
    let parsed = commands::claude::ClaudeArgs::parse_from(args);
    commands::claude::run(parsed).await
}
