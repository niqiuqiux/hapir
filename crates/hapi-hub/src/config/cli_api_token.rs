use super::settings::{read_settings, write_settings, settings_file_path};
use super::CliApiTokenSource;
use anyhow::Result;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::path::Path;

pub struct CliApiTokenResult {
    pub token: String,
    pub source: CliApiTokenSource,
    pub is_new: bool,
}

/// Generate a 32-byte base64url token (~43 chars).
fn generate_secure_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("failed to generate random bytes");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn is_weak_token(token: &str) -> bool {
    if token.len() < 16 {
        return true;
    }
    // Pure numbers
    if token.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // Repeated character
    if token.len() > 1 && token.chars().all(|c| c == token.chars().next().unwrap()) {
        return true;
    }
    // Common prefixes
    let lower = token.to_lowercase();
    if lower.starts_with("abc")
        || lower.starts_with("123")
        || lower.starts_with("password")
        || lower.starts_with("secret")
        || lower.starts_with("token")
    {
        return true;
    }
    false
}

/// Get or create CLI API token. Priority: env > file > generate.
pub fn get_or_create_cli_api_token(data_dir: &Path) -> Result<CliApiTokenResult> {
    let settings_path = settings_file_path(data_dir);

    // 1. Check env
    if let Ok(env_token) = std::env::var("CLI_API_TOKEN") {
        if !env_token.is_empty() {
            let token = strip_namespace_suffix(&env_token);
            if is_weak_token(&token) {
                tracing::warn!("CLI_API_TOKEN appears to be weak");
            }
            // Persist to file if not already saved
            if let Ok(Some(mut settings)) = read_settings(&settings_path) {
                if settings.cli_api_token.is_none() {
                    settings.cli_api_token = Some(token.clone());
                    let _ = write_settings(&settings_path, &settings);
                }
            }
            return Ok(CliApiTokenResult {
                token,
                source: CliApiTokenSource::Env,
                is_new: false,
            });
        }
    }

    // 2. Check file
    if let Ok(Some(settings)) = read_settings(&settings_path) {
        if let Some(ref file_token) = settings.cli_api_token {
            let token = strip_namespace_suffix(file_token);
            return Ok(CliApiTokenResult {
                token,
                source: CliApiTokenSource::File,
                is_new: false,
            });
        }
    }

    // 3. Generate
    let token = generate_secure_token();
    if let Ok(Some(mut settings)) = read_settings(&settings_path) {
        settings.cli_api_token = Some(token.clone());
        let _ = write_settings(&settings_path, &settings);
    } else {
        let mut settings = super::settings::Settings::default();
        settings.cli_api_token = Some(token.clone());
        let _ = write_settings(&settings_path, &settings);
    }

    Ok(CliApiTokenResult {
        token,
        source: CliApiTokenSource::Generated,
        is_new: true,
    })
}

/// Parsed access token: base token + namespace.
pub struct ParsedAccessToken {
    pub base_token: String,
    pub namespace: String,
}

/// Parse access token format: "baseToken:namespace" or just "baseToken" (default namespace).
pub fn parse_access_token(raw: &str) -> Option<ParsedAccessToken> {
    if raw.is_empty() {
        return None;
    }
    if let Some(pos) = raw.rfind(':') {
        let base = &raw[..pos];
        let ns = &raw[pos + 1..];
        if !base.is_empty() && !ns.is_empty() {
            return Some(ParsedAccessToken {
                base_token: base.to_string(),
                namespace: ns.to_string(),
            });
        }
    }
    Some(ParsedAccessToken {
        base_token: raw.to_string(),
        namespace: "default".to_string(),
    })
}

/// Constant-time comparison of two strings.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    a_bytes.len() == b_bytes.len() && a_bytes.ct_eq(b_bytes).unwrap_u8() == 1
}

fn strip_namespace_suffix(token: &str) -> String {
    // If token contains ':', strip the namespace suffix
    if let Some(pos) = token.rfind(':') {
        let base = &token[..pos];
        if !base.is_empty() {
            tracing::warn!("CLI_API_TOKEN includes namespace suffix, stripping it");
            return base.to_string();
        }
    }
    token.to_string()
}
