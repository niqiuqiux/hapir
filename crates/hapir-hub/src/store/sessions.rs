use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

use super::types::{StoredSession, VersionedUpdateResult};
use super::versioned_updates::update_versioned_field;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn safe_json_parse(value: Option<String>) -> Option<Value> {
    value.and_then(|s| serde_json::from_str(&s).ok())
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<StoredSession> {
    let active_int: i64 = row.get("active")?;
    Ok(StoredSession {
        id: row.get("id")?,
        tag: row.get("tag")?,
        namespace: row.get("namespace")?,
        machine_id: row.get("machine_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        metadata: safe_json_parse(row.get("metadata")?),
        metadata_version: row.get("metadata_version")?,
        agent_state: safe_json_parse(row.get("agent_state")?),
        agent_state_version: row.get("agent_state_version")?,
        todos: safe_json_parse(row.get("todos")?),
        todos_updated_at: row.get("todos_updated_at")?,
        active: active_int == 1,
        active_at: row.get("active_at")?,
        seq: row.get("seq")?,
    })
}

pub fn get_or_create_session(
    conn: &Connection,
    tag: &str,
    metadata: &Value,
    agent_state: Option<&Value>,
    namespace: &str,
) -> anyhow::Result<StoredSession> {
    // PLACEHOLDER_SESSIONS_CONTINUE
    let mut stmt = conn.prepare(
        "SELECT * FROM sessions WHERE tag = ?1 AND namespace = ?2 ORDER BY created_at DESC LIMIT 1",
    )?;
    let existing = stmt.query_row(rusqlite::params![tag, namespace], row_to_session);

    if let Ok(session) = existing {
        return Ok(session);
    }

    let now = now_millis();
    let id = format!("sess_{}", uuid::Uuid::new_v4());
    let metadata_json = serde_json::to_string(metadata)?;
    let agent_state_json = agent_state.map(|v| serde_json::to_string(v)).transpose()?;

    conn.execute(
        "INSERT INTO sessions (
            id, tag, namespace, machine_id, created_at, updated_at,
            metadata, metadata_version,
            agent_state, agent_state_version,
            todos, todos_updated_at,
            active, active_at, seq
        ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, 1, ?7, 1, NULL, NULL, 0, NULL, 0)",
        rusqlite::params![id, tag, namespace, now, now, metadata_json, agent_state_json],
    )?;

    get_session(conn, &id).ok_or_else(|| anyhow::anyhow!("failed to create session"))
}

pub fn update_session_metadata(
    conn: &Connection,
    id: &str,
    metadata: &Value,
    expected_version: i64,
    namespace: &str,
    touch_updated_at: bool,
) -> VersionedUpdateResult<Option<Value>> {
    let now = now_millis();
    let encoded = serde_json::to_string(metadata).ok();
    let touch_flag: i64 = if touch_updated_at { 1 } else { 0 };

    update_versioned_field(
        conn,
        "sessions",
        id,
        namespace,
        "metadata",
        "metadata_version",
        expected_version,
        encoded.as_deref(),
        &[
            "updated_at = CASE WHEN :touch_updated_at = 1 THEN :updated_at ELSE updated_at END".into(),
            "seq = seq + 1".into(),
        ],
        &[
            (":updated_at", &now as &dyn rusqlite::types::ToSql),
            (":touch_updated_at", &touch_flag),
        ],
    )
}

pub fn update_session_agent_state(
    conn: &Connection,
    id: &str,
    agent_state: Option<&Value>,
    expected_version: i64,
    namespace: &str,
) -> VersionedUpdateResult<Option<Value>> {
    let now = now_millis();
    let encoded = agent_state.and_then(|v| serde_json::to_string(v).ok());

    update_versioned_field(
        conn,
        "sessions",
        id,
        namespace,
        "agent_state",
        "agent_state_version",
        expected_version,
        encoded.as_deref(),
        &[
            "updated_at = :updated_at".into(),
            "seq = seq + 1".into(),
        ],
        &[(":updated_at", &now as &dyn rusqlite::types::ToSql)],
    )
}

pub fn set_session_todos(
    conn: &Connection,
    id: &str,
    todos: Option<&Value>,
    todos_updated_at: i64,
    namespace: &str,
) -> bool {
    let json = todos.and_then(|v| serde_json::to_string(v).ok());
    let result = conn.execute(
        "UPDATE sessions
         SET todos = ?1,
             todos_updated_at = ?2,
             updated_at = CASE WHEN updated_at > ?3 THEN updated_at ELSE ?3 END,
             seq = seq + 1
         WHERE id = ?4
           AND namespace = ?5
           AND (todos_updated_at IS NULL OR todos_updated_at < ?2)",
        rusqlite::params![json, todos_updated_at, todos_updated_at, id, namespace],
    );
    matches!(result, Ok(1))
}

pub fn get_session(conn: &Connection, id: &str) -> Option<StoredSession> {
    conn.prepare("SELECT * FROM sessions WHERE id = ?1")
        .ok()?
        .query_row(rusqlite::params![id], row_to_session)
        .ok()
}

pub fn get_session_by_namespace(
    conn: &Connection,
    id: &str,
    namespace: &str,
) -> Option<StoredSession> {
    conn.prepare("SELECT * FROM sessions WHERE id = ?1 AND namespace = ?2")
        .ok()?
        .query_row(rusqlite::params![id, namespace], row_to_session)
        .ok()
}

// PLACEHOLDER_SESSIONS_QUERIES

pub fn get_sessions(conn: &Connection) -> Vec<StoredSession> {
    let mut stmt = match conn.prepare("SELECT * FROM sessions ORDER BY updated_at DESC") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], row_to_session)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn get_sessions_by_namespace(conn: &Connection, namespace: &str) -> Vec<StoredSession> {
    let mut stmt = match conn.prepare(
        "SELECT * FROM sessions WHERE namespace = ?1 ORDER BY updated_at DESC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![namespace], row_to_session)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn delete_session(conn: &Connection, id: &str, namespace: &str) -> bool {
    let result = conn.execute(
        "DELETE FROM sessions WHERE id = ?1 AND namespace = ?2",
        rusqlite::params![id, namespace],
    );
    matches!(result, Ok(n) if n > 0)
}
