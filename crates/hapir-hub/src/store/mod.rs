pub mod machines;
pub mod messages;
pub mod push_subscriptions;
pub mod sessions;
pub mod types;
pub mod users;
pub mod versioned_updates;

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tracing::{debug, info, warn};

const SCHEMA_VERSION: i64 = 3;

const REQUIRED_TABLES: &[&str] = &[
    "sessions",
    "machines",
    "messages",
    "users",
    "push_subscriptions",
];

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn new(path: &str) -> Result<Self> {
        // Set directory and file permissions (matching TS behavior)
        let db_path = Path::new(path);
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| {
                format!("failed to create database directory {}", dir.display())
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }

        let conn =
            Connection::open(path).with_context(|| format!("failed to open database at {path}"))?;

        // Set file permissions on DB and WAL/SHM files
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in &["", "-wal", "-shm"] {
                let file_path = format!("{path}{suffix}");
                let _ =
                    std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600));
            }
        }

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.configure_pragmas()?;
        store.initialize_schema()?;

        Ok(store)
    }

    pub fn new_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory database")?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.configure_pragmas()?;
        store.initialize_schema()?;

        Ok(store)
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    fn configure_pragmas(&self) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute_batch(
                "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
            )
            .context("failed to configure database pragmas")?;

        debug!("database pragmas configured");
        Ok(())
    }

    fn get_schema_version(&self) -> Result<i64> {
        let version: i64 = self
            .conn
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .context("failed to read schema version")?;
        Ok(version)
    }

    fn set_schema_version(&self, version: i64) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .pragma_update(None, "user_version", version)
            .context("failed to set schema version")?;
        Ok(())
    }

    fn initialize_schema(&self) -> Result<()> {
        let current_version = self.get_schema_version()?;
        info!(
            current_version,
            target_version = SCHEMA_VERSION,
            "checking schema version"
        );

        if current_version == 0 {
            self.create_tables()?;
            self.set_schema_version(SCHEMA_VERSION)?;
            info!("created database schema v{SCHEMA_VERSION}");
            return Ok(());
        }

        if current_version < SCHEMA_VERSION {
            self.migrate_schema(current_version)?;
        }

        self.assert_required_tables()?;

        Ok(())
    }

    fn assert_required_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .context("failed to prepare table check query")?;

        let missing: Vec<&str> = REQUIRED_TABLES
            .iter()
            .filter(|&&table| !stmt.exists(rusqlite::params![table]).unwrap_or(false))
            .copied()
            .collect();

        if !missing.is_empty() {
            anyhow::bail!(
                "SQLite schema is missing required tables ({}). \
                 Back up and rebuild the database, or run an offline migration to the expected schema version.",
                missing.join(", ")
            );
        }

        Ok(())
    }

    fn create_tables(&self) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                tag TEXT,
                namespace TEXT NOT NULL DEFAULT 'default',
                machine_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                metadata TEXT,
                metadata_version INTEGER DEFAULT 1,
                agent_state TEXT,
                agent_state_version INTEGER DEFAULT 1,
                todos TEXT,
                todos_updated_at INTEGER,
                active INTEGER DEFAULT 0,
                active_at INTEGER,
                seq INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_tag ON sessions(tag);
            CREATE INDEX IF NOT EXISTS idx_sessions_tag_namespace ON sessions(tag, namespace);

            CREATE TABLE IF NOT EXISTS machines (
                id TEXT PRIMARY KEY,
                namespace TEXT NOT NULL DEFAULT 'default',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                metadata TEXT,
                metadata_version INTEGER DEFAULT 1,
                runner_state TEXT,
                runner_state_version INTEGER DEFAULT 1,
                active INTEGER DEFAULT 0,
                active_at INTEGER,
                seq INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_machines_namespace ON machines(namespace);

            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                seq INTEGER NOT NULL,
                local_id TEXT,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);",
            )
            .context("failed to create tables (part 1)")?;

        self.conn.lock().unwrap().execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_local_id
                ON messages(session_id, local_id) WHERE local_id IS NOT NULL;

            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                platform TEXT NOT NULL,
                platform_user_id TEXT NOT NULL,
                namespace TEXT NOT NULL DEFAULT 'default',
                created_at INTEGER NOT NULL,
                UNIQUE(platform, platform_user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_users_platform ON users(platform);
            CREATE INDEX IF NOT EXISTS idx_users_platform_namespace ON users(platform, namespace);

            CREATE TABLE IF NOT EXISTS push_subscriptions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                namespace TEXT NOT NULL,
                endpoint TEXT NOT NULL,
                p256dh TEXT NOT NULL,
                auth TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                UNIQUE(namespace, endpoint)
            );
            CREATE INDEX IF NOT EXISTS idx_push_subscriptions_namespace ON push_subscriptions(namespace);",
        ).context("failed to create tables (part 2)")?;

        Ok(())
    }

    fn migrate_schema(&self, from_version: i64) -> Result<()> {
        let mut version = from_version;

        while version < SCHEMA_VERSION {
            info!(from = version, to = version + 1, "migrating schema");

            match version {
                // 该项目是对hapi的翻译
                // 不需要兼任旧版本hapi的迁移了，直接升到最新版本就行
                // 1 => self.migrate_v1_to_v2()?,
                // 2 => self.migrate_v2_to_v3()?,
                _ => {
                    warn!(version, "unknown schema version, skipping");
                }
            }

            version += 1;
            self.set_schema_version(version)?;
        }

        info!(version = SCHEMA_VERSION, "schema migration complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_store() -> Store {
        Store::new_in_memory().unwrap()
    }

    #[test]
    fn store_creates_schema() {
        let store = test_store();
        let version = store.get_schema_version().unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn session_crud() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({"path": "/tmp/test", "host": "localhost"});

        let s = sessions::get_or_create_session(conn, "tag1", &meta, None, "default").unwrap();
        assert_eq!(s.tag.as_deref(), Some("tag1"));
        assert_eq!(s.namespace, "default");
        assert_eq!(s.metadata_version, 1);

        // Idempotent: same tag returns same session
        let s2 = sessions::get_or_create_session(conn, "tag1", &meta, None, "default").unwrap();
        assert_eq!(s.id, s2.id);

        // Get by id
        let s3 = sessions::get_session(conn, &s.id).unwrap();
        assert_eq!(s3.id, s.id);

        // Get by namespace
        let s4 = sessions::get_session_by_namespace(conn, &s.id, "default").unwrap();
        assert_eq!(s4.id, s.id);
        assert!(sessions::get_session_by_namespace(conn, &s.id, "other").is_none());

        // List
        let all = sessions::get_sessions(conn);
        assert_eq!(all.len(), 1);
        let by_ns = sessions::get_sessions_by_namespace(conn, "default");
        assert_eq!(by_ns.len(), 1);

        // Delete
        assert!(sessions::delete_session(conn, &s.id, "default"));
        assert!(sessions::get_session(conn, &s.id).is_none());
    }

    // PLACEHOLDER_STORE_TESTS_CONTINUE

    #[test]
    fn session_versioned_metadata_update() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({"path": "/tmp", "host": "h"});
        let s = sessions::get_or_create_session(conn, "t1", &meta, None, "default").unwrap();

        let new_meta = json!({"path": "/tmp/new", "host": "h"});
        let result = sessions::update_session_metadata(conn, &s.id, &new_meta, 1, "default", true);
        assert!(matches!(
            result,
            types::VersionedUpdateResult::Success { version: 2, .. }
        ));

        // Wrong version → mismatch
        let result2 = sessions::update_session_metadata(conn, &s.id, &new_meta, 1, "default", true);
        assert!(matches!(
            result2,
            types::VersionedUpdateResult::VersionMismatch { version: 2, .. }
        ));
    }

    #[test]
    fn session_agent_state_update() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({"path": "/tmp", "host": "h"});
        let state = json!({"controlledByUser": true});
        let s =
            sessions::get_or_create_session(conn, "t1", &meta, Some(&state), "default").unwrap();

        let new_state = json!({"controlledByUser": false});
        let result =
            sessions::update_session_agent_state(conn, &s.id, Some(&new_state), 1, "default");
        assert!(matches!(
            result,
            types::VersionedUpdateResult::Success { version: 2, .. }
        ));
    }

    #[test]
    fn session_todos() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({"path": "/tmp", "host": "h"});
        let s = sessions::get_or_create_session(conn, "t1", &meta, None, "default").unwrap();

        let todos =
            json!([{"content": "fix bug", "status": "pending", "priority": "high", "id": "t1"}]);
        assert!(sessions::set_session_todos(
            conn,
            &s.id,
            Some(&todos),
            1000,
            "default"
        ));

        // Older timestamp should not overwrite
        let todos2 = json!([]);
        assert!(!sessions::set_session_todos(
            conn,
            &s.id,
            Some(&todos2),
            500,
            "default"
        ));
    }

    #[test]
    fn machine_crud() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({"name": "my-machine"});

        let m = machines::get_or_create_machine(conn, "m1", &meta, None, "default").unwrap();
        assert_eq!(m.id, "m1");
        assert_eq!(m.namespace, "default");

        // Idempotent
        let m2 = machines::get_or_create_machine(conn, "m1", &meta, None, "default").unwrap();
        assert_eq!(m.id, m2.id);

        // Namespace mismatch
        let err = machines::get_or_create_machine(conn, "m1", &meta, None, "other");
        assert!(err.is_err());

        // List
        assert_eq!(machines::get_machines(conn).len(), 1);
        assert_eq!(
            machines::get_machines_by_namespace(conn, "default").len(),
            1
        );
    }

    #[test]
    fn machine_runner_state_sets_active() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({});
        let m = machines::get_or_create_machine(conn, "m1", &meta, None, "default").unwrap();
        assert!(!m.active);

        let state = json!({"pid": 1234});
        let result = machines::update_machine_runner_state(conn, "m1", Some(&state), 1, "default");
        assert!(matches!(
            result,
            types::VersionedUpdateResult::Success { .. }
        ));

        let m2 = machines::get_machine(conn, "m1").unwrap();
        assert!(m2.active);
        assert!(m2.active_at.is_some());
    }

    #[test]
    fn message_crud() {
        let store = test_store();
        let conn = &store.conn();
        let meta = json!({"path": "/tmp", "host": "h"});
        let s = sessions::get_or_create_session(conn, "t1", &meta, None, "default").unwrap();

        let msg1 = messages::add_message(
            conn,
            &s.id,
            &json!({"role": "user", "content": "hi"}),
            Some("l1"),
        )
        .unwrap();
        assert_eq!(msg1.seq, 1);
        assert_eq!(msg1.local_id.as_deref(), Some("l1"));

        // Idempotent by local_id
        let msg1b = messages::add_message(
            conn,
            &s.id,
            &json!({"role": "user", "content": "hi"}),
            Some("l1"),
        )
        .unwrap();
        assert_eq!(msg1.id, msg1b.id);

        let msg2 = messages::add_message(
            conn,
            &s.id,
            &json!({"role": "assistant", "content": "hello"}),
            None,
        )
        .unwrap();
        assert_eq!(msg2.seq, 2);

        // Get messages
        let all = messages::get_messages(conn, &s.id, 200, None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 1); // chronological order

        // Get with before_seq
        let before = messages::get_messages(conn, &s.id, 200, Some(2));
        assert_eq!(before.len(), 1);

        // Get after
        let after = messages::get_messages_after(conn, &s.id, 1, 200);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].seq, 2);
    }

    #[test]
    fn user_crud() {
        let store = test_store();
        let conn = &store.conn();

        let u = users::add_user(conn, "telegram", "123", "default").unwrap();
        assert_eq!(u.platform, "telegram");
        assert_eq!(u.platform_user_id, "123");

        // Idempotent
        let u2 = users::add_user(conn, "telegram", "123", "default").unwrap();
        assert_eq!(u.id, u2.id);

        // Get
        let found = users::get_user(conn, "telegram", "123").unwrap();
        assert_eq!(found.id, u.id);

        // List
        assert_eq!(users::get_users_by_platform(conn, "telegram").len(), 1);
        assert_eq!(
            users::get_users_by_platform_and_namespace(conn, "telegram", "default").len(),
            1
        );

        // Remove
        assert!(users::remove_user(conn, "telegram", "123"));
        assert!(users::get_user(conn, "telegram", "123").is_none());
    }

    #[test]
    fn push_subscription_crud() {
        let store = test_store();
        let conn = &store.conn();

        let sub = push_subscriptions::PushSubscriptionInput {
            endpoint: "https://push.example.com/1",
            p256dh: "key1",
            auth: "auth1",
        };
        push_subscriptions::add_push_subscription(conn, "default", &sub);

        let subs = push_subscriptions::get_push_subscriptions_by_namespace(conn, "default");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].endpoint, "https://push.example.com/1");

        // Upsert
        let sub2 = push_subscriptions::PushSubscriptionInput {
            endpoint: "https://push.example.com/1",
            p256dh: "key2",
            auth: "auth2",
        };
        push_subscriptions::add_push_subscription(conn, "default", &sub2);
        let subs2 = push_subscriptions::get_push_subscriptions_by_namespace(conn, "default");
        assert_eq!(subs2.len(), 1);
        assert_eq!(subs2[0].p256dh, "key2");

        // Remove
        push_subscriptions::remove_push_subscription(conn, "default", "https://push.example.com/1");
        assert!(
            push_subscriptions::get_push_subscriptions_by_namespace(conn, "default").is_empty()
        );
    }
}
