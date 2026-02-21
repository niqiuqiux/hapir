use anyhow::{Result, bail};
use clap::Parser;
use tracing::{debug, error};

use crate::commands::common;
use hapir_infra::config::Configuration;
use crate::modules::claude::run::{StartOptions, run_claude};

/// Run the default (claude) command.
///
/// Mirrors the TypeScript `claude.ts` entry point: parse args, initialize
/// token, register machine, ensure runner, then hand off to `run_claude`.
pub async fn run(args: ClaudeArgs) -> Result<()> {
    debug!(?args, "claude command starting");

    let mut config = Configuration::create()?;

    // Map --yolo / --dangerously-skip-permissions to permission mode
    let permission_mode = if args.dangerously_skip_permissions {
        Some("dangerouslySkipPermissions".to_string())
    } else if args.yolo {
        Some("bypassPermissions".to_string())
    } else {
        None
    };

    // Full init: token, machine, runner
    let runner_port = match common::full_init(&mut config).await {
        Ok(port) => port,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("CLI_API_TOKEN") {
                error!("Authentication required. Run 'hapir auth login' first.");
                bail!("{e}");
            }
            if msg.contains("failed to register machine") {
                error!("Could not reach the hub. Is it running? Check 'hapir doctor'.");
                bail!("{e}");
            }
            return Err(e);
        }
    };

    let options = StartOptions {
        model: args.model,
        permission_mode,
        starting_mode: args.hapir_starting_mode,
        should_start_runner: Some(true),
        claude_env_vars: None,
        claude_args: if args.passthrough_args.is_empty() {
            None
        } else {
            Some(args.passthrough_args)
        },
        started_by: args.started_by,
        runner_port,
    };

    run_claude(options).await
}

/// Parsed arguments for the claude (default) command.
#[derive(Parser, Debug, Default)]
#[command(name = "claude")]
pub struct ClaudeArgs {
    /// Starting mode for the session
    #[arg(long)]
    pub hapir_starting_mode: Option<String>,

    /// Bypass permission prompts
    #[arg(long)]
    pub yolo: bool,

    /// Skip all permission checks (dangerous)
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    /// Model to use
    #[arg(long)]
    pub model: Option<String>,

    /// What started this session
    #[arg(long)]
    pub started_by: Option<String>,

    /// Extra arguments passed through to claude
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub passthrough_args: Vec<String>,
}
