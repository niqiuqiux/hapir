use anyhow::{bail, Result};
use tracing::{debug, error};

use crate::commands::common;
use crate::config::Configuration;
use crate::modules::claude::run::{run_claude, StartOptions};

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
    let _runner_port = match common::full_init(&mut config).await {
        Ok(port) => port,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("CLI_API_TOKEN") {
                error!("Authentication required. Run 'hapi auth login' first.");
                bail!("{e}");
            }
            if msg.contains("failed to register machine") {
                error!("Could not reach the hub. Is it running? Check 'hapi doctor'.");
                bail!("{e}");
            }
            return Err(e);
        }
    };

    let options = StartOptions {
        model: args.model,
        permission_mode,
        starting_mode: args.hapi_starting_mode,
        should_start_runner: Some(true),
        claude_env_vars: None,
        claude_args: if args.passthrough_args.is_empty() {
            None
        } else {
            Some(args.passthrough_args)
        },
        started_by: args.started_by,
    };

    run_claude(options).await
}

/// Parsed arguments for the claude (default) command.
#[derive(Debug, Default)]
pub struct ClaudeArgs {
    pub hapi_starting_mode: Option<String>,
    pub yolo: bool,
    pub dangerously_skip_permissions: bool,
    pub model: Option<String>,
    pub started_by: Option<String>,
    pub passthrough_args: Vec<String>,
}

impl ClaudeArgs {
    /// Parse from raw CLI args (everything after the subcommand, or all args
    /// when no subcommand is given).
    pub fn parse_from(args: Vec<String>) -> Self {
        let mut result = Self::default();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--hapi-starting-mode" => {
                    result.hapi_starting_mode = iter.next();
                }
                "--yolo" => result.yolo = true,
                "--dangerously-skip-permissions" => {
                    result.dangerously_skip_permissions = true;
                }
                "--model" => {
                    result.model = iter.next();
                }
                "--started-by" => {
                    result.started_by = iter.next();
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    // Check for --key=value forms
                    if let Some(val) = other.strip_prefix("--hapi-starting-mode=") {
                        result.hapi_starting_mode = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--model=") {
                        result.model = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--started-by=") {
                        result.started_by = Some(val.to_string());
                    } else {
                        result.passthrough_args.push(other.to_string());
                    }
                }
            }
        }

        result
    }
}

fn print_help() {
    eprintln!(
        r#"hapi claude - Start a Claude agent session (default command)

Usage:
  hapi [claude] [options] [-- <claude-args>...]

Options:
  --hapi-starting-mode <mode>    Starting mode for the session
  --yolo                         Bypass permission prompts
  --dangerously-skip-permissions Skip all permission checks (dangerous)
  --model <model>                Model to use
  --started-by <source>          What started this session
  -h, --help                     Show this help
"#
    );
}
