use std::io::{self, BufRead, Write};

use crate::config::Configuration;
use crate::persistence;

pub fn run(action: Option<&str>) -> anyhow::Result<()> {
    let config = Configuration::create()?;

    match action {
        Some("status") => show_status(&config),
        Some("login") => login(&config),
        Some("logout") => logout(&config),
        _ => {
            show_help();
            Ok(())
        }
    }
}

fn show_status(config: &Configuration) -> anyhow::Result<()> {
    let settings = persistence::read_settings(&config.settings_file)?;

    let env_token = std::env::var("CLI_API_TOKEN").ok();
    let settings_token = settings.cli_api_token.as_ref();
    let has_token = env_token.is_some() || settings_token.is_some();
    let token_source = if env_token.is_some() {
        "environment"
    } else if settings_token.is_some() {
        "settings file"
    } else {
        "none"
    };

    let hostname = hostname();

    println!("\nDirect Connect Status\n");
    println!("  HAPI_API_URL: {}", config.api_url);
    println!("  CLI_API_TOKEN: {}", if has_token { "set" } else { "missing" });
    println!("  Token Source: {token_source}");
    println!(
        "  Machine ID: {}",
        settings.machine_id.as_deref().unwrap_or("not set")
    );
    println!("  Host: {hostname}");

    if !has_token {
        println!();
        println!("  Token not configured. To get your token:");
        println!("    1. Check the server startup logs (first run shows generated token)");
        println!("    2. Read ~/.hapi/settings.json on the server");
        println!("    3. Ask your server administrator (if token is set via env var)");
        println!();
        println!("  Then run: hapi auth login");
    }

    Ok(())
}

fn login(config: &Configuration) -> anyhow::Result<()> {
    if !atty::is(atty::Stream::Stdin) {
        eprintln!("Cannot prompt for token in non-TTY environment.");
        eprintln!("Set CLI_API_TOKEN environment variable instead.");
        std::process::exit(1);
    }

    print!("Enter CLI_API_TOKEN: ");
    io::stdout().flush()?;

    let stdin = io::stdin();
    let token = stdin.lock().lines().next()
        .ok_or_else(|| anyhow::anyhow!("no input"))??;
    let token = token.trim().to_string();

    if token.is_empty() {
        eprintln!("Token cannot be empty");
        std::process::exit(1);
    }

    persistence::update_settings(&config.settings_file, |s| {
        s.cli_api_token = Some(token.clone());
    })?;

    println!("\nToken saved to {}", config.settings_file.display());
    Ok(())
}

fn logout(config: &Configuration) -> anyhow::Result<()> {
    persistence::update_settings(&config.settings_file, |s| {
        s.cli_api_token = None;
        s.machine_id = None;
    })?;

    println!("Cleared local credentials (token and machineId).");
    println!("Note: If CLI_API_TOKEN is set via environment variable, it will still be used.");
    Ok(())
}

fn show_help() {
    println!(
        r#"
hapi auth - Authentication management

Usage:
  hapi auth status            Show current configuration
  hapi auth login             Enter and save CLI_API_TOKEN
  hapi auth logout            Clear saved credentials

Token priority (highest to lowest):
  1. CLI_API_TOKEN environment variable
  2. ~/.hapi/settings.json
  3. Interactive prompt (on first run)
"#
    );
}

fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) } == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..len]).to_string()
        } else {
            "unknown".into()
        }
    }
    #[cfg(not(unix))]
    {
        "unknown".into()
    }
}
