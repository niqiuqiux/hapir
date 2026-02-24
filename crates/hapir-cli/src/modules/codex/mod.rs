use sha2::{Digest, Sha256};

mod session_scanner;
pub mod run;

#[derive(Debug, Clone, Default)]
pub struct CodexMode {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub collaboration_mode: Option<String>,
}

fn compute_mode_hash(mode: &CodexMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.model.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.collaboration_mode.as_deref().unwrap_or(""));
    hex::encode(hasher.finalize())
}
