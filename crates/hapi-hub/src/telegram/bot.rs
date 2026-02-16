use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{error, info, warn};

use hapi_shared::schemas::Session;

use crate::notifications::push_channel::NotificationChannel;
use crate::notifications::session_info::get_agent_name;
use crate::store::users;
use crate::store::Store;
use crate::sync::SyncEngine;

use super::api::{InlineKeyboardButton, InlineKeyboardMarkup, TelegramApi, WebAppInfo};
use super::callbacks::handle_callback;
use super::renderer::build_mini_app_deep_link;
use super::session_view::{create_notification_keyboard, format_session_notification};

pub struct HappyBot {
    api: Arc<TelegramApi>,
    sync_engine: Arc<Mutex<SyncEngine>>,
    store: Arc<Store>,
    public_url: String,
    running: Arc<AtomicBool>,
}

impl HappyBot {
    pub fn new(
        bot_token: &str,
        sync_engine: Arc<Mutex<SyncEngine>>,
        store: Arc<Store>,
        public_url: String,
    ) -> Self {
        Self {
            api: Arc::new(TelegramApi::new(bot_token)),
            sync_engine,
            store,
            public_url,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the long-polling loop in a background task.
    pub fn start(&self) {
        if self.running.load(Ordering::SeqCst) {
            return;
        }
        self.running.store(true, Ordering::SeqCst);
        info!("starting Telegram bot");

        let api = self.api.clone();
        let sync_engine = self.sync_engine.clone();
        let store = self.store.clone();
        let public_url = self.public_url.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            let mut offset: Option<i64> = None;
            while running.load(Ordering::SeqCst) {
                let updates = match api.get_updates(offset, 30).await {
                    Ok(u) => u,
                    Err(e) => {
                        warn!(error = %e, "Telegram getUpdates failed");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                for update in &updates {
                    offset = Some(update.update_id + 1);

                    // Handle commands
                    if let Some(ref msg) = update.message {
                        if let Some(ref text) = msg.text {
                            let chat_id = msg.chat.id;
                            if text.starts_with("/start") || text.starts_with("/app") {
                                let keyboard = InlineKeyboardMarkup {
                                    inline_keyboard: vec![vec![InlineKeyboardButton {
                                        text: "Open App".into(),
                                        callback_data: None,
                                        web_app: Some(WebAppInfo {
                                            url: public_url.clone(),
                                        }),
                                    }]],
                                };
                                let reply = if text.starts_with("/start") {
                                    "Welcome to HAPI Bot!\n\nUse the Mini App for full session management."
                                } else {
                                    "Open HAPI Mini App:"
                                };
                                if let Err(e) =
                                    api.send_message(chat_id, reply, Some(&keyboard)).await
                                {
                                    error!(error = %e, "failed to reply to command");
                                }
                            }
                        }
                    }

                    // Handle callback queries
                    if let Some(ref cq) = update.callback_query {
                        let Some(ref data) = cq.data else {
                            continue;
                        };

                        let namespace = {
                            let conn = store.conn();
                            users::get_user(&conn, "telegram", &cq.from.id.to_string())
                                .map(|u| u.namespace)
                        };

                        let Some(namespace) = namespace else {
                            let _ = api
                                .answer_callback_query(&cq.id, Some("Telegram account is not bound"))
                                .await;
                            continue;
                        };

                        let (chat_id, message_id) = cq
                            .message
                            .as_ref()
                            .map(|m| (m.chat.id, m.message_id))
                            .unwrap_or((0, 0));

                        handle_callback(
                            data,
                            &sync_engine,
                            &namespace,
                            &api,
                            chat_id,
                            message_id,
                            &cq.id,
                        )
                        .await;
                    }
                }
            }
            info!("Telegram bot polling stopped");
        });
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn get_bound_chat_ids(&self, namespace: &str) -> Vec<i64> {
        let conn = self.store.conn();
        users::get_users_by_platform_and_namespace(&conn, "telegram", namespace)
            .iter()
            .filter_map(|u| u.platform_user_id.parse::<i64>().ok())
            .collect()
    }
}

impl NotificationChannel for HappyBot {
    fn send_ready<'a>(
        &'a self,
        session: &'a Session,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if !session.active {
                return Ok(());
            }

            let agent_name = get_agent_name(session);
            let url = build_mini_app_deep_link(
                &self.public_url,
                &format!("session_{}", session.id),
            );
            let keyboard = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text: "Open Session".into(),
                    callback_data: None,
                    web_app: Some(WebAppInfo { url }),
                }]],
            };

            let chat_ids = self.get_bound_chat_ids(&session.namespace);
            for chat_id in chat_ids {
                let text = format!(
                    "It's ready!\n\n{} is waiting for your command",
                    agent_name
                );
                if let Err(e) = self.api.send_message(chat_id, &text, Some(&keyboard)).await {
                    error!(chat_id, error = %e, "failed to send ready notification");
                }
            }
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

            let text = format_session_notification(session);
            let keyboard = create_notification_keyboard(session, &self.public_url);

            let chat_ids = self.get_bound_chat_ids(&session.namespace);
            for chat_id in chat_ids {
                if let Err(e) = self.api.send_message(chat_id, &text, Some(&keyboard)).await {
                    error!(chat_id, error = %e, "failed to send permission notification");
                }
            }
            Ok(())
        })
    }
}
