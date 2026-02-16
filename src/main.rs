use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hapi", about = "Local-first AI agent remote control")]
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
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Start a Codex agent session
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Start a Gemini agent session
    Gemini {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Start an OpenCode agent session
    Opencode {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Claude { args }) => hapi_cli::run_cli(args).await,
        Some(Commands::Codex { args }) => {
            let parsed = hapi_cli::commands::codex::CodexArgs::parse_from(args);
            hapi_cli::commands::codex::run(parsed).await
        }
        Some(Commands::Gemini { args }) => {
            let parsed = hapi_cli::commands::gemini::GeminiArgs::parse_from(args);
            hapi_cli::commands::gemini::run(parsed).await
        }
        Some(Commands::Opencode { args }) => {
            let parsed = hapi_cli::commands::opencode::OpencodeArgs::parse_from(args);
            hapi_cli::commands::opencode::run(parsed).await
        }
        Some(Commands::Mcp { args }) => hapi_cli::commands::mcp::run(args).await,
        Some(Commands::HookForwarder { args }) => {
            hapi_cli::commands::hook_forwarder::run(args).await
        }
        Some(Commands::Hub) => hapi_hub::run_hub().await,
        Some(Commands::Auth { action }) => {
            hapi_cli::commands::auth::run(action.map(|a| match a {
                AuthAction::Status => "status",
                AuthAction::Login => "login",
                AuthAction::Logout => "logout",
            }))
        }
        Some(Commands::Runner { action }) => {
            let sub = action.map(|a| match a {
                RunnerAction::Start => "start",
                RunnerAction::StartSync => "start-sync",
                RunnerAction::Stop => "stop",
                RunnerAction::Status => "status",
                RunnerAction::Logs => "logs",
                RunnerAction::List => "list",
            });
            hapi_cli::commands::runner::run(sub).await
        }
        Some(Commands::Doctor) => hapi_cli::commands::doctor::run(),
        // No subcommand: default to claude with any trailing args
        None => hapi_cli::run_cli(cli.args).await,
    }
}
