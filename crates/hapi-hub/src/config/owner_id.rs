use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Serialize, Deserialize)]
struct OwnerIdFile {
    #[serde(rename = "ownerId")]
    owner_id: i64,
}

static OWNER_ID: OnceLock<i64> = OnceLock::new();

/// Get or create a persistent owner ID. The value is cached in memory after the
/// first successful read/generation.
///
/// The ID is a random 48-bit positive integer stored in `{data_dir}/owner-id.json`.
pub fn get_or_create_owner_id(data_dir: &Path) -> Result<i64> {
    if let Some(&id) = OWNER_ID.get() {
        return Ok(id);
    }

    let id = load_or_generate(data_dir)?;
    // Another thread may have initialised the lock in the meantime; that is
    // fine -- we just return whichever value won the race.
    let _ = OWNER_ID.set(id);
    Ok(*OWNER_ID.get().unwrap())
}

fn load_or_generate(data_dir: &Path) -> Result<i64> {
    let path = data_dir.join("owner-id.json");

    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        let file: OwnerIdFile = serde_json::from_str(&content)?;
        if file.owner_id <= 0 {
            anyhow::bail!("invalid owner ID in {}", path.display());
        }
        return Ok(file.owner_id);
    }

    // Generate a random 48-bit positive integer from 6 random bytes.
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("failed to generate random bytes: {e}"))?;

    let mut id: i64 = 0;
    for &b in &bytes {
        id = (id << 8) | (b as i64);
    }

    // Ensure the value is strictly positive.
    if id <= 0 {
        id = id.wrapping_abs();
        if id <= 0 {
            id = 1;
        }
    }

    let file = OwnerIdFile { owner_id: id };
    let json = serde_json::to_string_pretty(&file)?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // Write the file.
    std::fs::write(&path, &json)?;

    // Restrict permissions to owner-only on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn generates_owner_id_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = load_or_generate(dir.path()).unwrap();
        assert!(id > 0);
        assert!(id < (1i64 << 48));

        // File should exist now.
        let path = dir.path().join("owner-id.json");
        assert!(path.exists());

        // Re-reading should return the same value.
        let id2 = load_or_generate(dir.path()).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn reads_existing_owner_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner-id.json");
        fs::write(&path, r#"{"ownerId": 42}"#).unwrap();

        let id = load_or_generate(dir.path()).unwrap();
        assert_eq!(id, 42);
    }

    #[test]
    fn rejects_non_positive_owner_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner-id.json");
        fs::write(&path, r#"{"ownerId": 0}"#).unwrap();

        let result = load_or_generate(dir.path());
        assert!(result.is_err());
    }
}
