use anyhow::Result;
use clap::Parser;
use tracing::debug;

use crate::commands::common;
use hapir_infra::config::CliConfiguration;

/// Parsed arguments for the opencode command.
#[derive(Parser, Debug, Default)]
#[command(name = "opencode")]
pub struct OpencodeArgs {
    /// What started this session
    #[arg(long)]
    pub started_by: Option<String>,

    /// Starting mode for the session
    #[arg(long)]
    pub hapir_starting_mode: Option<String>,

    /// Bypass permission prompts
    #[arg(long)]
    pub yolo: bool,

    /// Resume an existing session
    #[arg(long)]
    pub resume: Option<String>,
}

/// Run the opencode command.
pub async fn run(args: OpencodeArgs) -> Result<()> {
    debug!(?args, "opencode command starting");

    let mut config = CliConfiguration::new()?;
    let runner_port = common::full_init(&mut config).await?;

    let working_directory = std::env::current_dir()?.to_string_lossy().to_string();

    crate::modules::opencode::run::run(&working_directory, runner_port).await
}
