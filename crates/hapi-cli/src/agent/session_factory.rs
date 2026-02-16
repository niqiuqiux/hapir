use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use hapi_shared::schemas::{Metadata, Session, StartedBy};

use crate::api::ApiClient;
use crate::config::Configuration;
use crate::persistence;
use crate::ws::session_client::WsSessionClient;

/// Machine-level metadata sent when registering a machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineMetadata {
    pub host: String,
    pub platform: String,
    pub happy_cli_version: String,
    pub home_dir: String,
    pub happy_home_dir: String,
    pub happy_lib_dir: String,
}

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

/// Build machine-level metadata from the current environment.
pub fn build_machine_metadata(config: &Configuration) -> MachineMetadata {
    let hostname = std::env::var("HAPI_HOSTNAME")
        .unwrap_or_else(|_| gethostname().to_string_lossy().to_string());
    let home_dir = dirs_next::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    MachineMetadata {
        host: hostname,
        platform: std::env::consts::OS.to_string(),
        happy_cli_version: env!("CARGO_PKG_VERSION").to_string(),
        home_dir,
        happy_home_dir: config.home_dir.to_string_lossy().to_string(),
        happy_lib_dir: config.home_dir.to_string_lossy().to_string(),
    }
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
        os: Some(std::env::consts::OS.to_string()),
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

/// Read the machine ID from settings, or bail.
fn get_machine_id(config: &Configuration) -> anyhow::Result<String> {
    let settings = persistence::read_settings(&config.settings_file)?;
    settings
        .machine_id
        .ok_or_else(|| anyhow::anyhow!("No machine ID found in settings. Run 'hapi auth login' first."))
}

/// Bootstrap a session: create machine, create session, connect WS client.
pub async fn bootstrap_session(
    opts: SessionBootstrapOptions,
    config: &Configuration,
) -> anyhow::Result<SessionBootstrapResult> {
    let working_directory = opts
        .working_directory
        .unwrap_or_else(|| std::env::current_dir().unwrap().to_string_lossy().to_string());
    let started_by = opts.started_by.unwrap_or(StartedBy::Terminal);
    let session_tag = opts.tag.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let agent_state = opts.agent_state;

    let api = Arc::new(ApiClient::new(config)?);

    let machine_id = get_machine_id(config)?;
    debug!("Using machineId: {machine_id}");

    let machine_meta = build_machine_metadata(config);
    api.get_or_create_machine(&machine_id, &serde_json::to_value(&machine_meta)?, None)
        .await?;

    let metadata = build_session_metadata(&opts.flavor, started_by, &working_directory, &machine_id, config);

    let session_info = api
        .get_or_create_session(&session_tag, &metadata, agent_state.as_ref())
        .await?;

    let ws_client = Arc::new(WsSessionClient::new(
        api.base_url(),
        api.token(),
        &session_info,
    ));
    ws_client.connect().await;

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

fn gethostname() -> std::ffi::OsString {
    #[cfg(unix)]
    {
        let mut buf = vec![0u8; 256];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if ret == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            std::ffi::OsString::from(String::from_utf8_lossy(&buf[..len]).to_string())
        } else {
            std::ffi::OsString::from("unknown")
        }
    }
    #[cfg(not(unix))]
    {
        std::ffi::OsString::from("unknown")
    }
}
