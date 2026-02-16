use anyhow::Result;
use tracing::debug;

use crate::commands::common;
use crate::config::Configuration;

/// Parsed arguments for the codex command.
#[derive(Debug, Default)]
pub struct CodexArgs {
    pub resume: Option<String>,
    pub started_by: Option<String>,
    pub yolo: bool,
    pub model: Option<String>,
    pub passthrough_args: Vec<String>,
}

impl CodexArgs {
    pub fn parse_from(args: Vec<String>) -> Self {
        let mut result = Self::default();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--started-by" => result.started_by = iter.next(),
                "--yolo" => result.yolo = true,
                "--model" => result.model = iter.next(),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    if let Some(val) = other.strip_prefix("--started-by=") {
                        result.started_by = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--model=") {
                        result.model = Some(val.to_string());
                    } else if !other.starts_with('-') && result.resume.is_none() {
                        // First positional arg is the resume session ID
                        result.resume = Some(other.to_string());
                    } else {
                        result.passthrough_args.push(other.to_string());
                    }
                }
            }
        }

        result
    }
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

fn print_help() {
    eprintln!(
        r#"hapi codex - Start a Codex agent session

Usage:
  hapi codex [session-id] [options]

Arguments:
  [session-id]          Resume an existing session

Options:
  --started-by <source> What started this session
  --yolo                Bypass permission prompts
  --model <model>       Model to use
  -h, --help            Show this help
"#
    );
}
