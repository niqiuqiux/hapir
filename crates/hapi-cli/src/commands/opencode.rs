use anyhow::Result;
use tracing::debug;

use crate::commands::common;
use crate::config::Configuration;

/// Parsed arguments for the opencode command.
#[derive(Debug, Default)]
pub struct OpencodeArgs {
    pub started_by: Option<String>,
    pub hapi_starting_mode: Option<String>,
    pub yolo: bool,
    pub resume: Option<String>,
}

impl OpencodeArgs {
    pub fn parse_from(args: Vec<String>) -> Self {
        let mut result = Self::default();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--started-by" => result.started_by = iter.next(),
                "--hapi-starting-mode" => result.hapi_starting_mode = iter.next(),
                "--yolo" => result.yolo = true,
                "--resume" => result.resume = iter.next(),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    if let Some(val) = other.strip_prefix("--started-by=") {
                        result.started_by = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--hapi-starting-mode=") {
                        result.hapi_starting_mode = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--resume=") {
                        result.resume = Some(val.to_string());
                    }
                }
            }
        }

        result
    }
}

/// Run the opencode command.
pub async fn run(args: OpencodeArgs) -> Result<()> {
    debug!(?args, "opencode command starting");

    let mut config = Configuration::create()?;
    let _runner_port = common::full_init(&mut config).await?;

    let working_directory = std::env::current_dir()?
        .to_string_lossy()
        .to_string();

    crate::modules::opencode::run(&working_directory).await
}

fn print_help() {
    eprintln!(
        r#"hapi opencode - Start an OpenCode agent session

Usage:
  hapi opencode [options]

Options:
  --started-by <source>          What started this session
  --hapi-starting-mode <mode>    Starting mode for the session
  --yolo                         Bypass permission prompts
  --resume <session-id>          Resume an existing session
  -h, --help                     Show this help
"#
    );
}
