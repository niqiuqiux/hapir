use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::types::StoredUser;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn row_to_user(row: &rusqlite::Row) -> rusqlite::Result<StoredUser> {
    Ok(StoredUser {
        id: row.get("id")?,
        platform: row.get("platform")?,
        platform_user_id: row.get("platform_user_id")?,
        namespace: row.get("namespace")?,
        created_at: row.get("created_at")?,
    })
}

pub fn get_user(conn: &Connection, platform: &str, platform_user_id: &str) -> Option<StoredUser> {
    conn.prepare("SELECT * FROM users WHERE platform = ?1 AND platform_user_id = ?2 LIMIT 1")
        .ok()?
        .query_row(rusqlite::params![platform, platform_user_id], row_to_user)
        .ok()
}

pub fn get_users_by_platform(conn: &Connection, platform: &str) -> Vec<StoredUser> {
    let mut stmt =
        match conn.prepare("SELECT * FROM users WHERE platform = ?1 ORDER BY created_at ASC") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
    stmt.query_map(rusqlite::params![platform], row_to_user)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn get_users_by_platform_and_namespace(
    conn: &Connection,
    platform: &str,
    namespace: &str,
) -> Vec<StoredUser> {
    let mut stmt = match conn.prepare(
        "SELECT * FROM users WHERE platform = ?1 AND namespace = ?2 ORDER BY created_at ASC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![platform, namespace], row_to_user)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn add_user(
    conn: &Connection,
    platform: &str,
    platform_user_id: &str,
    namespace: &str,
) -> anyhow::Result<StoredUser> {
    let now = now_millis();
    conn.execute(
        "INSERT OR IGNORE INTO users (platform, platform_user_id, namespace, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![platform, platform_user_id, namespace, now],
    )?;

    get_user(conn, platform, platform_user_id)
        .ok_or_else(|| anyhow::anyhow!("failed to create user"))
}

pub fn remove_user(conn: &Connection, platform: &str, platform_user_id: &str) -> bool {
    let result = conn.execute(
        "DELETE FROM users WHERE platform = ?1 AND platform_user_id = ?2",
        rusqlite::params![platform, platform_user_id],
    );
    matches!(result, Ok(n) if n > 0)
}
