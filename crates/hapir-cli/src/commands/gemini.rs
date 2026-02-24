use anyhow::Result;
use clap::Parser;
use tracing::debug;

use crate::commands::common;
use hapir_infra::config::CliConfiguration;

/// Parsed arguments for the gemini command.
#[derive(Parser, Debug, Default)]
#[command(name = "gemini")]
pub struct GeminiArgs {
    /// What started this session
    #[arg(long)]
    pub started_by: Option<String>,

    /// Starting mode for the session
    #[arg(long)]
    pub hapir_starting_mode: Option<String>,

    /// Bypass permission prompts
    #[arg(long)]
    pub yolo: bool,

    /// Model to use
    #[arg(long)]
    pub model: Option<String>,
}

/// Run the gemini command.
pub async fn run(args: GeminiArgs) -> Result<()> {
    debug!(?args, "gemini command starting");

    let mut config = CliConfiguration::new()?;
    let runner_port = common::full_init(&mut config).await?;

    let working_directory = std::env::current_dir()?.to_string_lossy().to_string();

    crate::modules::gemini::run::run(&working_directory, runner_port).await
}
