use anyhow::Result;
use tracing::debug;

use crate::commands::common;
use crate::config::Configuration;

/// Parsed arguments for the gemini command.
#[derive(Debug, Default)]
pub struct GeminiArgs {
    pub started_by: Option<String>,
    pub hapi_starting_mode: Option<String>,
    pub yolo: bool,
    pub model: Option<String>,
}

impl GeminiArgs {
    pub fn parse_from(args: Vec<String>) -> Self {
        let mut result = Self::default();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--started-by" => result.started_by = iter.next(),
                "--hapi-starting-mode" => result.hapi_starting_mode = iter.next(),
                "--yolo" => result.yolo = true,
                "--model" => result.model = iter.next(),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    if let Some(val) = other.strip_prefix("--started-by=") {
                        result.started_by = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--hapi-starting-mode=") {
                        result.hapi_starting_mode = Some(val.to_string());
                    } else if let Some(val) = other.strip_prefix("--model=") {
                        result.model = Some(val.to_string());
                    }
                }
            }
        }

        result
    }
}

/// Run the gemini command.
pub async fn run(args: GeminiArgs) -> Result<()> {
    debug!(?args, "gemini command starting");

    let mut config = Configuration::create()?;
    let _runner_port = common::full_init(&mut config).await?;

    let working_directory = std::env::current_dir()?
        .to_string_lossy()
        .to_string();

    crate::modules::gemini::run(&working_directory).await
}

fn print_help() {
    eprintln!(
        r#"hapi gemini - Start a Gemini agent session

Usage:
  hapi gemini [options]

Options:
  --started-by <source>          What started this session
  --hapi-starting-mode <mode>    Starting mode for the session
  --yolo                         Bypass permission prompts
  --model <model>                Model to use
  -h, --help                     Show this help
"#
    );
}
