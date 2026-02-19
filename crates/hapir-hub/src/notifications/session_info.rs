use hapir_shared::schemas::Session;

/// Returns a human-readable name for the session.
///
/// Tries, in order:
/// 1. `metadata.name`
/// 2. `metadata.summary.text`
/// 3. Last path component from `metadata.path`
/// 4. First 8 characters of session ID
pub fn get_session_name(session: &Session) -> String {
    if let Some(ref metadata) = session.metadata {
        // Explicit name
        if let Some(ref name) = metadata.name
            && !name.is_empty()
        {
            return name.clone();
        }

        // Summary text
        if let Some(ref summary) = metadata.summary
            && !summary.text.is_empty()
        {
            return summary.text.clone();
        }

        // Last path component
        let parts: Vec<&str> = metadata.path.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(last) = parts.last() {
            return (*last).to_string();
        }
    }

    // Fallback: first 8 chars of session ID
    session.id.chars().take(8).collect()
}

/// Returns a human-readable agent name based on the session flavor.
pub fn get_agent_name(session: &Session) -> String {
    let flavor = session
        .metadata
        .as_ref()
        .and_then(|m| m.flavor.as_deref());

    match flavor {
        Some("claude") => "Claude".to_string(),
        Some("codex") => "Codex".to_string(),
        Some("gemini") => "Gemini".to_string(),
        Some("opencode") => "OpenCode".to_string(),
        _ => "Agent".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hapir_shared::schemas::{Metadata, MetadataSummary};

    fn base_session() -> Session {
        Session {
            id: "abcdefgh12345678".to_string(),
            namespace: "default".to_string(),
            seq: 0.0,
            created_at: 0.0,
            updated_at: 0.0,
            active: true,
            active_at: 0.0,
            metadata: None,
            metadata_version: 1.0,
            agent_state: None,
            agent_state_version: 0.0,
            thinking: false,
            thinking_at: 0.0,
            todos: None,
            permission_mode: None,
            model_mode: None,
        }
    }

    fn base_metadata() -> Metadata {
        Metadata {
            path: String::new(),
            host: "localhost".to_string(),
            version: None,
            name: None,
            os: None,
            summary: None,
            machine_id: None,
            claude_session_id: None,
            codex_session_id: None,
            gemini_session_id: None,
            opencode_session_id: None,
            tools: None,
            slash_commands: None,
            home_dir: None,
            happy_home_dir: None,
            happy_lib_dir: None,
            happy_tools_dir: None,
            started_from_runner: None,
            host_pid: None,
            started_by: None,
            lifecycle_state: None,
            lifecycle_state_since: None,
            archived_by: None,
            archive_reason: None,
            flavor: None,
            worktree: None,
        }
    }

    #[test]
    fn session_name_from_metadata_name() {
        let mut session = base_session();
        let mut meta = base_metadata();
        meta.name = Some("my-session".to_string());
        session.metadata = Some(meta);
        assert_eq!(get_session_name(&session), "my-session");
    }

    #[test]
    fn session_name_from_summary() {
        let mut session = base_session();
        let mut meta = base_metadata();
        meta.summary = Some(MetadataSummary {
            text: "A summary".to_string(),
            updated_at: 0.0,
        });
        session.metadata = Some(meta);
        assert_eq!(get_session_name(&session), "A summary");
    }

    #[test]
    fn session_name_from_path() {
        let mut session = base_session();
        let mut meta = base_metadata();
        meta.path = "/home/user/my-project".to_string();
        session.metadata = Some(meta);
        assert_eq!(get_session_name(&session), "my-project");
    }

    #[test]
    fn session_name_fallback_to_id() {
        let session = base_session();
        assert_eq!(get_session_name(&session), "abcdefgh");
    }

    #[test]
    fn agent_name_claude() {
        let mut session = base_session();
        let mut meta = base_metadata();
        meta.flavor = Some("claude".to_string());
        session.metadata = Some(meta);
        assert_eq!(get_agent_name(&session), "Claude");
    }

    #[test]
    fn agent_name_codex() {
        let mut session = base_session();
        let mut meta = base_metadata();
        meta.flavor = Some("codex".to_string());
        session.metadata = Some(meta);
        assert_eq!(get_agent_name(&session), "Codex");
    }

    #[test]
    fn agent_name_default() {
        let session = base_session();
        assert_eq!(get_agent_name(&session), "Agent");
    }
}
