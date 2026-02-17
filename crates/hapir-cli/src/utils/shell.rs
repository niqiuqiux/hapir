use std::collections::HashMap;

/// Env var keys that should be filtered out when spawning child processes.
const SENSITIVE_ENV_KEYS: &[&str] = &[
    "CLI_API_TOKEN",
    "HAPIR_API_URL",
    "HAPIR_HTTP_MCP_URL",
    "TELEGRAM_BOT_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// Return the list of sensitive environment variable keys.
pub fn sensitive_env_keys() -> &'static [&'static str] {
    SENSITIVE_ENV_KEYS
}

/// Resolve the default shell for the current platform.
///
/// Checks `$SHELL` first, then falls back to `/bin/zsh` on macOS
/// or `/bin/bash` elsewhere.
pub fn default_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return shell;
        }
    }

    if cfg!(target_os = "macos") {
        "/bin/zsh".to_string()
    } else {
        "/bin/bash".to_string()
    }
}

/// Return the current environment with sensitive keys removed.
pub fn filtered_env() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| !SENSITIVE_ENV_KEYS.contains(&key.as_str()))
        .collect()
}
