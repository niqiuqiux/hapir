use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hapir_shared::schemas::{Session, SyncEvent, ToastData};
use tokio::sync::Mutex;
use crate::sync::SyncEngine;

use super::push_service::{PushData, PushPayload, PushService};
use super::session_info::{get_agent_name, get_session_name};

/// Trait for notification delivery channels.
///
/// Uses `Pin<Box<dyn Future>>` return types for object safety, enabling
/// `dyn NotificationChannel` in collections.
pub trait NotificationChannel: Send + Sync {
    fn send_ready<'a>(
        &'a self,
        session: &'a Session,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

    fn send_permission_request<'a>(
        &'a self,
        session: &'a Session,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;
}

/// Push notification channel that delivers notifications via web push,
/// with SSE toast as a preferred fast path when a visible connection exists.
pub struct PushNotificationChannel {
    push_service: Arc<PushService>,
    sync_engine: Arc<Mutex<SyncEngine>>,
    #[allow(dead_code)]
    app_url: String,
}

impl PushNotificationChannel {
    pub fn new(
        push_service: Arc<PushService>,
        sync_engine: Arc<Mutex<SyncEngine>>,
        app_url: String,
    ) -> Self {
        Self {
            push_service,
            sync_engine,
            app_url,
        }
    }

    fn build_session_path(session_id: &str) -> String {
        format!("/sessions/{session_id}")
    }

    /// Attempt to deliver a toast notification via SSE to visible connections.
    /// Returns the number of connections that received the toast.
    async fn try_send_toast(
        &self,
        namespace: &str,
        title: &str,
        body: &str,
        session_id: &str,
        url: &str,
    ) -> usize {
        let engine = self.sync_engine.lock().await;

        if !engine.sse_manager().visibility().has_visible_connection(namespace) {
            return 0;
        }

        let toast_event = SyncEvent::Toast {
            namespace: Some(namespace.to_string()),
            data: ToastData {
                title: title.to_string(),
                body: body.to_string(),
                session_id: session_id.to_string(),
                url: url.to_string(),
            },
        };

        engine.sse_manager().send_toast(namespace, &toast_event)
    }
}

impl NotificationChannel for PushNotificationChannel {
    fn send_ready<'a>(
        &'a self,
        session: &'a Session,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if !session.active {
                return Ok(());
            }

            let agent_name = get_agent_name(session);
            let session_name = get_session_name(session);
            let url = Self::build_session_path(&session.id);

            let payload = PushPayload {
                title: "Ready for input".to_string(),
                body: format!("{agent_name} is waiting in {session_name}"),
                tag: Some(format!("ready-{}", session.id)),
                data: Some(PushData {
                    r#type: "ready".to_string(),
                    session_id: session.id.clone(),
                    url: url.clone(),
                }),
            };

            // Try SSE toast first
            let delivered = self
                .try_send_toast(
                    &session.namespace,
                    &payload.title,
                    &payload.body,
                    &session.id,
                    &url,
                )
                .await;

            if delivered > 0 {
                return Ok(());
            }

            // Fall back to push notification
            self.push_service
                .send_to_namespace(&session.namespace, &payload)
                .await;

            Ok(())
        })
    }

    fn send_permission_request<'a>(
        &'a self,
        session: &'a Session,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if !session.active {
                return Ok(());
            }

            let session_name = get_session_name(session);
            let request = session
                .agent_state
                .as_ref()
                .and_then(|s| s.requests.as_ref())
                .and_then(|reqs| reqs.values().next());
            let tool_name = match request {
                Some(req) if !req.tool.is_empty() => format!(" ({})", req.tool),
                _ => String::new(),
            };

            let url = Self::build_session_path(&session.id);

            let payload = PushPayload {
                title: "Permission Request".to_string(),
                body: format!("{session_name}{tool_name}"),
                tag: Some(format!("permission-{}", session.id)),
                data: Some(PushData {
                    r#type: "permission-request".to_string(),
                    session_id: session.id.clone(),
                    url: url.clone(),
                }),
            };

            // Try SSE toast first
            let delivered = self
                .try_send_toast(
                    &session.namespace,
                    &payload.title,
                    &payload.body,
                    &session.id,
                    &url,
                )
                .await;

            if delivered > 0 {
                return Ok(());
            }

            // Fall back to push notification
            self.push_service
                .send_to_namespace(&session.namespace, &payload)
                .await;

            Ok(())
        })
    }
}
