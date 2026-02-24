use sha2::{Digest, Sha256};

pub mod run;

/// The mode type for Gemini sessions.
#[derive(Debug, Clone, Default)]
pub struct GeminiMode {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
}

/// Compute a deterministic hash of the gemini mode for queue batching.
fn compute_mode_hash(mode: &GeminiMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.model.as_deref().unwrap_or(""));
    hex::encode(hasher.finalize())
}
