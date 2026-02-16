use anyhow::Result;
use clap::Parser;
use tracing::debug;

use crate::commands::common;
use crate::config::Configuration;

/// Parsed arguments for the codex command.
#[derive(Parser, Debug, Default)]
#[command(name = "codex")]
pub struct CodexArgs {
    /// Resume an existing session
    pub resume: Option<String>,

    /// What started this session
    #[arg(long)]
    pub started_by: Option<String>,

    /// Bypass permission prompts
    #[arg(long)]
    pub yolo: bool,

    /// Model to use
    #[arg(long)]
    pub model: Option<String>,
}

/// Run the codex command.
pub async fn run(args: CodexArgs) -> Result<()> {
    debug!(?args, "codex command starting");

    let mut config = Configuration::create()?;
    let _runner_port = common::full_init(&mut config).await?;

    let working_directory = std::env::current_dir()?
        .to_string_lossy()
        .to_string();

    crate::modules::codex::run(&working_directory).await
}
