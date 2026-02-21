use std::env::consts::OS;
use std::path::Path;
use std::sync::Arc;

use tracing::debug;

use hapir_shared::schemas::{Metadata, Session, StartedBy};

use hapir_infra::api::ApiClient;
use hapir_infra::config::Configuration;
use hapir_infra::machine::{build_machine_metadata, gethostname};
use hapir_infra::persistence;
use hapir_infra::ws::session_client::WsSessionClient;

pub use hapir_infra::machine::MachineMetadata;

/// Options for bootstrapping a session.
pub struct SessionBootstrapOptions {
    pub flavor: String,
    pub started_by: Option<StartedBy>,
    pub working_directory: Option<String>,
    pub tag: Option<String>,
    pub agent_state: Option<serde_json::Value>,
}

/// Result of bootstrapping a session.
pub struct SessionBootstrapResult {
    pub api: Arc<ApiClient>,
    pub ws_client: Arc<WsSessionClient>,
    pub session_info: Session,
    pub metadata: Metadata,
    pub machine_id: String,
    pub started_by: StartedBy,
    pub working_directory: String,
}

/// Build session metadata for a new session.
pub fn build_session_metadata(
    flavor: &str,
    started_by: StartedBy,
    working_directory: &str,
    machine_id: &str,
    config: &Configuration,
) -> Metadata {
    let hostname = gethostname().to_string_lossy().to_string();
    let home_dir = dirs_next::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let happy_home_dir = config.home_dir.to_string_lossy().to_string();
    let happy_lib_dir = happy_home_dir.clone();
    let happy_tools_dir = Path::new(&happy_lib_dir)
        .join("tools")
        .join("unpacked")
        .to_string_lossy()
        .to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;

    Metadata {
        path: working_directory.to_string(),
        host: hostname,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        name: None,
        os: Some(OS.to_string()),
        summary: None,
        machine_id: Some(machine_id.to_string()),
        claude_session_id: None,
        codex_session_id: None,
        gemini_session_id: None,
        opencode_session_id: None,
        tools: None,
        slash_commands: None,
        home_dir: Some(home_dir),
        happy_home_dir: Some(happy_home_dir),
        happy_lib_dir: Some(happy_lib_dir),
        happy_tools_dir: Some(happy_tools_dir),
        started_from_runner: Some(started_by == StartedBy::Runner),
        host_pid: Some(std::process::id() as f64),
        started_by: Some(started_by),
        lifecycle_state: Some("running".to_string()),
        lifecycle_state_since: Some(now),
        archived_by: None,
        archive_reason: None,
        flavor: Some(flavor.to_string()),
        worktree: None,
    }
}

// PLACEHOLDER_REST

/// Read the machine ID from settings, or bail.
fn get_machine_id(config: &Configuration) -> anyhow::Result<String> {
    let settings = persistence::read_settings(&config.settings_file)?;
    settings.machine_id.ok_or_else(|| {
        anyhow::anyhow!("No machine ID found in settings. Run 'hapir auth login' first.")
    })
}

/// Bootstrap a session: create machine, create session, connect WS client.
pub async fn bootstrap_session(
    opts: SessionBootstrapOptions,
    config: &Configuration,
) -> anyhow::Result<SessionBootstrapResult> {
    let working_directory = opts.working_directory.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string()
    });
    let started_by = opts.started_by.unwrap_or(StartedBy::Terminal);
    let session_tag = opts.tag.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let agent_state = opts.agent_state;

    let api = Arc::new(ApiClient::new(config)?);

    let machine_id = get_machine_id(config)?;
    debug!("Using machineId: {machine_id}");

    let machine_meta = build_machine_metadata(config);
    api.get_or_create_machine(&machine_id, &serde_json::to_value(&machine_meta)?, None)
        .await?;

    let metadata = build_session_metadata(
        &opts.flavor,
        started_by,
        &working_directory,
        &machine_id,
        config,
    );

    let session_info = api
        .get_or_create_session(&session_tag, &metadata, agent_state.as_ref())
        .await?;

    let ws_client = Arc::new(WsSessionClient::new(
        api.base_url(),
        api.token(),
        &session_info,
    ));

    Ok(SessionBootstrapResult {
        api,
        ws_client,
        session_info,
        metadata,
        machine_id,
        started_by,
        working_directory,
    })
}
