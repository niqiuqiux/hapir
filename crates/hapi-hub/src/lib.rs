pub mod config;
pub mod notifications;
pub mod store;
pub mod sync;
pub mod telegram;
pub mod web;
pub mod ws;

use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tracing::info;

use config::Configuration;
use notifications::notification_hub::NotificationHub;
use notifications::push_channel::{NotificationChannel, PushNotificationChannel};
use notifications::push_service::PushService;
use store::Store;
use sync::SyncEngine;
use web::AppState;
use ws::connection_manager::ConnectionManager;
use ws::terminal_registry::TerminalRegistry;
use ws::WsState;

pub async fn run_hub() -> anyhow::Result<()> {
    // 1. Load configuration
    let config = Configuration::create().await?;

    info!(
        port = config.listen_port,
        host = %config.listen_host,
        public_url = %config.public_url,
        telegram = config.telegram_enabled,
        "starting hub"
    );

    // 2. Create store
    let db_path_str = config.db_path.to_string_lossy().to_string();
    let store = Arc::new(Store::new(&db_path_str)?);

    // 3. Get/create JWT secret
    let jwt_secret = config::jwt_secret::get_or_create_jwt_secret(&config.data_dir)?;

    // 4. Get/create VAPID keys (optional — push notifications disabled if unavailable)
    let vapid_keys = config::vapid_keys::get_or_create_vapid_keys(&config.data_dir).ok();
    let vapid_public_key = vapid_keys.as_ref().map(|k| k.public_key.clone());

    // 5. Create connection manager
    let conn_mgr = Arc::new(ConnectionManager::new());

    // 6. Create SyncEngine
    let sync_engine = Arc::new(Mutex::new(
        SyncEngine::new(store.clone(), conn_mgr.clone()),
    ));

    // 7. Set up notification channels
    let mut notification_channels: Vec<Arc<dyn NotificationChannel>> = Vec::new();

    if let Some(ref keys) = vapid_keys {
        let vapid_subject = std::env::var("VAPID_SUBJECT")
            .unwrap_or_else(|_| "mailto:admin@localhost".into());
        let push_service = Arc::new(PushService::new(
            keys.clone(),
            vapid_subject,
            store.clone(),
        ));
        let push_channel = Arc::new(PushNotificationChannel::new(
            push_service,
            sync_engine.clone(),
            config.public_url.clone(),
        ));
        notification_channels.push(push_channel);
    }

    // 7b. Initialize Telegram bot (optional)
    let happy_bot = if config.telegram_enabled {
        if let Some(ref token) = config.telegram_bot_token {
            let bot = Arc::new(telegram::bot::HappyBot::new(
                token,
                sync_engine.clone(),
                store.clone(),
                config.public_url.clone(),
            ));
            if config.telegram_notification {
                notification_channels.push(bot.clone());
            }
            Some(bot)
        } else {
            None
        }
    } else {
        None
    };

    // 8. Start notification hub
    let notification_hub = Arc::new(NotificationHub::new(
        notification_channels,
        30_000, // ready cooldown ms
        3_000,  // permission debounce ms
    ));
    notification_hub.clone().start(sync_engine.clone()).await;

    // 9. Build AppState for HTTP routes
    let app_state = AppState {
        jwt_secret: jwt_secret.clone(),
        cli_api_token: config.cli_api_token.clone(),
        sync_engine: sync_engine.clone(),
        store: store.clone(),
        vapid_public_key,
        telegram_bot_token: config.telegram_bot_token.clone(),
        data_dir: config.data_dir.clone(),
    };

    // 10. Build WsState for WebSocket handlers
    let terminal_registry = Arc::new(RwLock::new(TerminalRegistry::new(15 * 60_000)));
    let ws_state = WsState {
        store: store.clone(),
        sync_engine: sync_engine.clone(),
        conn_mgr: conn_mgr.clone(),
        terminal_registry,
        cli_api_token: config.cli_api_token.clone(),
        jwt_secret,
        max_terminals_per_socket: 4,
        max_terminals_per_session: 4,
    };

    // 11. Build combined router
    let web_router = web::build_router(app_state);
    let ws_router = ws::ws_router(ws_state);
    let app = web_router.merge(ws_router);

    // 12. Start periodic expiration timer
    let sync_for_expire = sync_engine.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            sync_for_expire.lock().await.expire_inactive();
        }
    });

    // 13. Start SSE heartbeat timer
    let sync_for_heartbeat = sync_engine.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            sync_for_heartbeat.lock().await.sse_manager_mut().send_heartbeats();
        }
    });

    // 14. Display startup info
    if config.cli_api_token_is_new {
        info!(token = %config.cli_api_token, "generated new CLI API token");
    }

    // 15. Bind and serve
    let addr = format!("{}:{}", config.listen_host, config.listen_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "listening");

    // 16. Start Telegram bot (after HTTP server is ready)
    if let Some(ref bot) = happy_bot {
        bot.start();
    }

    info!(url = %config.public_url, "hub ready");

    // 17. Serve until shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // 18. Graceful shutdown: stop bot
    if let Some(ref bot) = happy_bot {
        bot.stop();
    }

    info!("hub stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("shutdown signal received");
}
