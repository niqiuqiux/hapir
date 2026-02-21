use std::io::IsTerminal;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use hapir_cli::commands::claude::ClaudeArgs;
use hapir_cli::commands::codex::CodexArgs;
use hapir_cli::commands::gemini::GeminiArgs;
use hapir_cli::commands::opencode::OpencodeArgs;

#[derive(Parser)]
#[command(name = "hapir", about = "Local-first AI agent remote control")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Remaining args passed to the default (claude) command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a Claude agent session (default)
    Claude(ClaudeArgs),

    /// Start a Codex agent session
    Codex(CodexArgs),

    /// Start a Gemini agent session
    Gemini(GeminiArgs),

    /// Start an OpenCode agent session
    Opencode(OpencodeArgs),

    /// Run the MCP stdio bridge
    Mcp {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Internal: forward hook events to the runner
    HookForwarder {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Start the hub server
    Hub,

    /// Authentication management
    Auth {
        #[command(subcommand)]
        action: Option<AuthAction>,
    },

    /// Runner process management
    Runner {
        #[command(subcommand)]
        action: Option<RunnerAction>,
    },

    /// Show diagnostics information
    Doctor,
}

#[derive(Subcommand)]
enum AuthAction {
    /// Show current configuration
    Status,
    /// Enter and save CLI_API_TOKEN
    Login,
    /// Clear saved credentials
    Logout,
}

#[derive(Subcommand)]
enum RunnerAction {
    /// Start runner in background
    Start,
    /// Start runner synchronously (foreground)
    StartSync,
    /// Stop runner
    Stop,
    /// Show runner status
    Status,
    /// Show latest runner logs
    Logs,
    /// List active sessions
    List,
}

fn build_cli() -> clap::Command {
    let (about, subs) = hapir_shared::i18n::cli_about_strings();
    let mut cmd = Cli::command().about(about);
    for sub in subs {
        cmd = cmd.mut_subcommand(sub.name, |c| {
            let mut c = c.about(sub.about);
            for (child_name, child_about) in sub.children {
                c = c.mut_subcommand(child_name, |sc| sc.about(child_about));
            }
            c
        });
    }
    cmd
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut cmd_for_help = build_cli();
    cmd_for_help.build();
    let matches = build_cli().get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("clap derive mismatch");

    let result = match cli.command {
        Some(Commands::Claude(args)) => hapir_cli::run_cli(args).await,
        Some(Commands::Codex(args)) => hapir_cli::commands::codex::run(args).await,
        Some(Commands::Gemini(args)) => hapir_cli::commands::gemini::run(args).await,
        Some(Commands::Opencode(args)) => hapir_cli::commands::opencode::run(args).await,
        Some(Commands::Mcp { args }) => hapir_cli::commands::mcp::run(args).await,
        Some(Commands::HookForwarder { args }) => {
            hapir_cli::commands::hook_forwarder::run(args).await
        }
        Some(Commands::Hub) => hapir_hub::run_hub().await,
        Some(Commands::Auth { action }) => match action {
            Some(a) => hapir_cli::commands::auth::run(Some(match a {
                AuthAction::Status => "status",
                AuthAction::Login => "login",
                AuthAction::Logout => "logout",
            })),
            None => {
                let _ = cmd_for_help
                    .find_subcommand_mut("auth")
                    .expect("auth subcommand")
                    .print_help();
                Ok(())
            }
        },
        Some(Commands::Runner { action }) => match action {
            Some(a) => {
                let sub = match a {
                    RunnerAction::Start => "start",
                    RunnerAction::StartSync => "start-sync",
                    RunnerAction::Stop => "stop",
                    RunnerAction::Status => "status",
                    RunnerAction::Logs => "logs",
                    RunnerAction::List => "list",
                };
                hapir_cli::commands::runner::run(Some(sub)).await
            }
            None => {
                let _ = cmd_for_help
                    .find_subcommand_mut("runner")
                    .expect("runner subcommand")
                    .print_help();
                Ok(())
            }
        },
        Some(Commands::Doctor) => hapir_cli::commands::doctor::run(),
        // No subcommand: default to claude with any trailing args
        None => {
            let claude_args =
                ClaudeArgs::try_parse_from(std::iter::once("hapir".to_string()).chain(cli.args))
                    .unwrap_or_else(|e| e.exit());
            hapir_cli::run_cli(claude_args).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
