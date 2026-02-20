use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;
use uuid::Uuid;
use super::types::StoredMessage;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn safe_json_parse(value: Option<String>) -> Option<Value> {
    value.and_then(|s| serde_json::from_str(&s).ok())
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        content: safe_json_parse(row.get("content")?),
        created_at: row.get("created_at")?,
        seq: row.get("seq")?,
        local_id: row.get("local_id")?,
    })
}

pub fn add_message(
    conn: &Connection,
    session_id: &str,
    content: &Value,
    local_id: Option<&str>,
) -> anyhow::Result<StoredMessage> {
    // Idempotent: check for existing by local_id
    if let Some(lid) = local_id {
        let mut stmt = conn.prepare(
            "SELECT * FROM messages WHERE session_id = ?1 AND local_id = ?2 LIMIT 1",
        )?;
        if let Ok(msg) = stmt.query_row(rusqlite::params![session_id, lid], row_to_message) {
            return Ok(msg);
        }
    }

    // Get next seq
    let next_seq: i64 = conn
        .prepare("SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1")?
        .query_row(rusqlite::params![session_id], |row| row.get(0))?;

    let now = now_millis();
    let id = Uuid::new_v4().to_string();
    let json = serde_json::to_string(content)?;

    conn.execute(
        "INSERT INTO messages (id, session_id, content, created_at, seq, local_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, session_id, json, now, next_seq, local_id],
    )?;

    conn.prepare("SELECT * FROM messages WHERE id = ?1")?
        .query_row(rusqlite::params![id], row_to_message)
        .map_err(|e| anyhow::anyhow!("failed to read created message: {e}"))
}

pub fn get_messages(
    conn: &Connection,
    session_id: &str,
    limit: i64,
    before_seq: Option<i64>,
) -> Vec<StoredMessage> {
    let safe_limit = limit.clamp(1, 200);

    let mut msgs = if let Some(seq) = before_seq {
        let mut stmt = match conn.prepare(
            "SELECT * FROM messages WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(rusqlite::params![session_id, seq, safe_limit], row_to_message)
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    } else {
        let mut stmt = match conn.prepare(
            "SELECT * FROM messages WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(rusqlite::params![session_id, safe_limit], row_to_message)
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    };

    msgs.reverse(); // Return in chronological order
    msgs
}

pub fn get_messages_after(
    conn: &Connection,
    session_id: &str,
    after_seq: i64,
    limit: i64,
) -> Vec<StoredMessage> {
    let safe_limit = limit.clamp(1, 200);
    let safe_after = if after_seq < 0 { 0 } else { after_seq };

    let mut stmt = match conn.prepare(
        "SELECT * FROM messages WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(
        rusqlite::params![session_id, safe_after, safe_limit],
        row_to_message,
    )
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn get_max_seq(conn: &Connection, session_id: &str) -> i64 {
    conn.prepare("SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1")
        .and_then(|mut s| s.query_row(rusqlite::params![session_id], |row| row.get(0)))
        .unwrap_or(0)
}

pub fn merge_session_messages(
    conn: &Connection,
    from_session_id: &str,
    to_session_id: &str,
) -> anyhow::Result<(i64, i64, i64)> {
    if from_session_id == to_session_id {
        return Ok((0, 0, 0));
    }

    let old_max = get_max_seq(conn, from_session_id);
    let new_max = get_max_seq(conn, to_session_id);

    conn.execute_batch("BEGIN")?;

    let result = (|| -> anyhow::Result<i64> {
        // Offset existing target seqs
        if new_max > 0 && old_max > 0 {
            conn.execute(
                "UPDATE messages SET seq = seq + ?1 WHERE session_id = ?2",
                rusqlite::params![old_max, to_session_id],
            )?;
        }

        // Find local_id collisions
        let mut collision_stmt = conn.prepare(
            "SELECT local_id FROM messages WHERE session_id = ?1 AND local_id IS NOT NULL
             INTERSECT
             SELECT local_id FROM messages WHERE session_id = ?2 AND local_id IS NOT NULL",
        )?;
        let collisions: Vec<String> = collision_stmt
            .query_map(rusqlite::params![to_session_id, from_session_id], |row| {
                row.get(0)
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Null out colliding local_ids in source
        for lid in &collisions {
            conn.execute(
                "UPDATE messages SET local_id = NULL WHERE session_id = ?1 AND local_id = ?2",
                rusqlite::params![from_session_id, lid],
            )?;
        }

        // Move all messages
        let moved = conn.execute(
            "UPDATE messages SET session_id = ?1 WHERE session_id = ?2",
            rusqlite::params![to_session_id, from_session_id],
        )?;

        Ok(moved as i64)
    })();

    match result {
        Ok(moved) => {
            conn.execute_batch("COMMIT")?;
            Ok((moved, old_max, new_max))
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}
