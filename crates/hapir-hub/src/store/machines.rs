use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

use super::types::{StoredMachine, VersionedUpdateResult};
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

fn row_to_machine(row: &rusqlite::Row) -> rusqlite::Result<StoredMachine> {
    let active_int: i64 = row.get("active")?;
    Ok(StoredMachine {
        id: row.get("id")?,
        namespace: row.get("namespace")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        metadata: safe_json_parse(row.get("metadata")?),
        metadata_version: row.get("metadata_version")?,
        runner_state: safe_json_parse(row.get("runner_state")?),
        runner_state_version: row.get("runner_state_version")?,
        active: active_int == 1,
        active_at: row.get("active_at")?,
        seq: row.get("seq")?,
    })
}

pub fn get_or_create_machine(
    conn: &Connection,
    id: &str,
    metadata: &Value,
    runner_state: Option<&Value>,
    namespace: &str,
) -> anyhow::Result<StoredMachine> {
    let mut stmt = conn.prepare("SELECT * FROM machines WHERE id = ?1")?;
    let existing = stmt.query_row(rusqlite::params![id], row_to_machine);

    if let Ok(machine) = existing {
        if machine.namespace != namespace {
            anyhow::bail!("machine namespace mismatch");
        }
        // Update metadata if the caller provided non-empty metadata
        if metadata.as_object().is_some_and(|o| !o.is_empty()) {
            let now = now_millis();
            let metadata_json = serde_json::to_string(metadata)?;
            conn.execute(
                "UPDATE machines SET metadata = ?1, updated_at = ?2, seq = seq + 1 WHERE id = ?3",
                rusqlite::params![metadata_json, now, id],
            )?;
            return get_machine(conn, id)
                .ok_or_else(|| anyhow::anyhow!("failed to reload machine after metadata update"));
        }
        return Ok(machine);
    }

    let now = now_millis();
    let metadata_json = serde_json::to_string(metadata)?;
    let runner_state_json = runner_state
        .map(|v| serde_json::to_string(v))
        .transpose()?;

    conn.execute(
        "INSERT INTO machines (
            id, namespace, created_at, updated_at,
            metadata, metadata_version,
            runner_state, runner_state_version,
            active, active_at, seq
        ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, 1, 0, NULL, 0)",
        rusqlite::params![id, namespace, now, now, metadata_json, runner_state_json],
    )?;

    get_machine(conn, id).ok_or_else(|| anyhow::anyhow!("failed to create machine"))
}

pub fn update_machine_metadata(
    conn: &Connection,
    id: &str,
    metadata: &Value,
    expected_version: i64,
    namespace: &str,
) -> VersionedUpdateResult<Option<Value>> {
    let now = now_millis();
    let encoded = serde_json::to_string(metadata).ok();

    update_versioned_field(
        conn,
        "machines",
        id,
        namespace,
        "metadata",
        "metadata_version",
        expected_version,
        encoded.as_deref(),
        &["updated_at = :updated_at".into(), "seq = seq + 1".into()],
        &[(":updated_at", &now as &dyn rusqlite::types::ToSql)],
    )
}

pub fn update_machine_runner_state(
    conn: &Connection,
    id: &str,
    runner_state: Option<&Value>,
    expected_version: i64,
    namespace: &str,
) -> VersionedUpdateResult<Option<Value>> {
    let now = now_millis();
    let encoded = runner_state.and_then(|v| serde_json::to_string(v).ok());

    update_versioned_field(
        conn,
        "machines",
        id,
        namespace,
        "runner_state",
        "runner_state_version",
        expected_version,
        encoded.as_deref(),
        &[
            "updated_at = :updated_at".into(),
            "active = 1".into(),
            "active_at = :active_at".into(),
            "seq = seq + 1".into(),
        ],
        &[
            (":updated_at", &now as &dyn rusqlite::types::ToSql),
            (":active_at", &now),
        ],
    )
}

pub fn get_machine(conn: &Connection, id: &str) -> Option<StoredMachine> {
    conn.prepare("SELECT * FROM machines WHERE id = ?1")
        .ok()?
        .query_row(rusqlite::params![id], row_to_machine)
        .ok()
}

pub fn get_machine_by_namespace(
    conn: &Connection,
    id: &str,
    namespace: &str,
) -> Option<StoredMachine> {
    conn.prepare("SELECT * FROM machines WHERE id = ?1 AND namespace = ?2")
        .ok()?
        .query_row(rusqlite::params![id, namespace], row_to_machine)
        .ok()
}

pub fn get_machines(conn: &Connection) -> Vec<StoredMachine> {
    let mut stmt = match conn.prepare("SELECT * FROM machines ORDER BY updated_at DESC") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], row_to_machine)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn get_machines_by_namespace(conn: &Connection, namespace: &str) -> Vec<StoredMachine> {
    let mut stmt = match conn.prepare(
        "SELECT * FROM machines WHERE namespace = ?1 ORDER BY updated_at DESC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![namespace], row_to_machine)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}
