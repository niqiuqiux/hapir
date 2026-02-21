use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::types::StoredPushSubscription;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn row_to_push_sub(row: &rusqlite::Row) -> rusqlite::Result<StoredPushSubscription> {
    Ok(StoredPushSubscription {
        id: row.get("id")?,
        namespace: row.get("namespace")?,
        endpoint: row.get("endpoint")?,
        p256dh: row.get("p256dh")?,
        auth: row.get("auth")?,
        created_at: row.get("created_at")?,
    })
}

pub struct PushSubscriptionInput<'a> {
    pub endpoint: &'a str,
    pub p256dh: &'a str,
    pub auth: &'a str,
}

pub fn add_push_subscription(conn: &Connection, namespace: &str, sub: &PushSubscriptionInput) {
    let now = now_millis();
    let _ = conn.execute(
        "INSERT INTO push_subscriptions (namespace, endpoint, p256dh, auth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(namespace, endpoint)
         DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth, created_at = excluded.created_at",
        rusqlite::params![namespace, sub.endpoint, sub.p256dh, sub.auth, now],
    );
}

pub fn remove_push_subscription(conn: &Connection, namespace: &str, endpoint: &str) {
    let _ = conn.execute(
        "DELETE FROM push_subscriptions WHERE namespace = ?1 AND endpoint = ?2",
        rusqlite::params![namespace, endpoint],
    );
}

pub fn get_push_subscriptions_by_namespace(
    conn: &Connection,
    namespace: &str,
) -> Vec<StoredPushSubscription> {
    let mut stmt = match conn
        .prepare("SELECT * FROM push_subscriptions WHERE namespace = ?1 ORDER BY created_at DESC")
    {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![namespace], row_to_push_sub)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}
