use hapir_infra::config::Configuration;
use hapir_infra::persistence;

pub fn run() -> anyhow::Result<()> {
    let config = Configuration::create()?;
    let settings = persistence::read_settings(&config.settings_file)?;

    println!("HAPIR Doctor\n");
    println!("  Version: {}", env!("CARGO_PKG_VERSION"));
    println!("  Home Dir: {}", config.home_dir.display());
    println!("  Settings File: {}", config.settings_file.display());
    println!("  API URL: {}", config.api_url);
    println!(
        "  CLI API Token: {}",
        if config.cli_api_token.is_empty() {
            "not set"
        } else {
            "set"
        }
    );
    println!(
        "  Machine ID: {}",
        settings.machine_id.as_deref().unwrap_or("not set")
    );
    println!(
        "  Experimental: {}",
        if config.is_experimental {
            "enabled"
        } else {
            "disabled"
        }
    );

    // Check runner state
    if let Some(state) = persistence::read_runner_state(&config.runner_state_file) {
        println!("\n  Runner:");
        println!("    PID: {}", state.pid);
        println!("    HTTP Port: {}", state.http_port);
        println!("    Started: {}", state.start_time);
        println!("    CLI Version: {}", state.started_with_cli_version);
    } else {
        println!("\n  Runner: not running");
    }

    Ok(())
}
