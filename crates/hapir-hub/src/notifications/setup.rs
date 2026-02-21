use std::sync::Arc;

use crate::config::Configuration;
use crate::config::settings::VapidKeys;
use crate::notifications::notification_hub::NotificationHub;
use crate::notifications::push_channel::{NotificationChannel, PushNotificationChannel};
use crate::notifications::push_service::PushService;
use crate::store::Store;
use crate::sync::SyncEngine;
use crate::telegram::bot::HappyBot;

pub struct NotificationSetup {
    pub notification_hub: Arc<NotificationHub>,
    pub happy_bot: Option<Arc<HappyBot>>,
}

pub fn build(
    config: &Configuration,
    vapid_keys: Option<VapidKeys>,
    store: Arc<Store>,
    sync_engine: Arc<SyncEngine>,
) -> NotificationSetup {
    let mut channels: Vec<Arc<dyn NotificationChannel>> = Vec::new();

    if let Some(ref keys) = vapid_keys {
        let vapid_subject =
            std::env::var("VAPID_SUBJECT").unwrap_or_else(|_| "mailto:admin@localhost".into());
        let push_service = Arc::new(PushService::new(keys.clone(), vapid_subject, store.clone()));
        let push_channel = Arc::new(PushNotificationChannel::new(
            push_service,
            sync_engine.clone(),
            config.public_url.clone(),
        ));
        channels.push(push_channel);
    }

    let happy_bot = if config.telegram_enabled {
        if let Some(ref token) = config.telegram_bot_token {
            let bot = Arc::new(HappyBot::new(
                token,
                sync_engine.clone(),
                store.clone(),
                config.public_url.clone(),
            ));
            if config.telegram_notification {
                channels.push(bot.clone());
            }
            Some(bot)
        } else {
            None
        }
    } else {
        None
    };

    let notification_hub = Arc::new(NotificationHub::new(channels, 5_000, 500));

    NotificationSetup {
        notification_hub,
        happy_bot,
    }
}
