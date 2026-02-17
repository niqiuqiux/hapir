use serde_json::Value;

#[derive(Debug, Clone)]
pub struct StoredSession {
    pub id: String,
    pub tag: Option<String>,
    pub namespace: String,
    pub machine_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub metadata: Option<Value>,
    pub metadata_version: i64,
    pub agent_state: Option<Value>,
    pub agent_state_version: i64,
    pub todos: Option<Value>,
    pub todos_updated_at: Option<i64>,
    pub active: bool,
    pub active_at: Option<i64>,
    pub seq: i64,
}

#[derive(Debug, Clone)]
pub struct StoredMachine {
    pub id: String,
    pub namespace: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub metadata: Option<Value>,
    pub metadata_version: i64,
    pub runner_state: Option<Value>,
    pub runner_state_version: i64,
    pub active: bool,
    pub active_at: Option<i64>,
    pub seq: i64,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: String,
    pub session_id: String,
    pub content: Option<Value>,
    pub created_at: i64,
    pub seq: i64,
    pub local_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredUser {
    pub id: i64,
    pub platform: String,
    pub platform_user_id: String,
    pub namespace: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct StoredPushSubscription {
    pub id: i64,
    pub namespace: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub enum VersionedUpdateResult<T> {
    Success { version: i64, value: T },
    VersionMismatch { version: i64, value: T },
    Error,
}
