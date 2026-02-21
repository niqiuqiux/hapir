use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::fs::{Permissions, set_permissions};
use std::path::Path;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JwtSecretFile {
    secret_base64: String,
}

pub fn get_or_create_jwt_secret(data_dir: &Path) -> Result<Vec<u8>> {
    let path = data_dir.join("jwt-secret.json");
    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        let file: JwtSecretFile = serde_json::from_str(&content)?;
        let bytes = STANDARD.decode(&file.secret_base64)?;
        if bytes.len() != 32 {
            anyhow::bail!("invalid JWT secret length in {}", path.display());
        }
        return Ok(bytes);
    }

    // Generate new secret
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("failed to generate random bytes: {e}"))?;
    let file = JwtSecretFile {
        secret_base64: STANDARD.encode(bytes),
    };
    let json = serde_json::to_string_pretty(&file)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, json)?;

    // Set file permissions to 0600 (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        set_permissions(&path, Permissions::from_mode(0o600))?;
    }

    Ok(bytes.to_vec())
}
