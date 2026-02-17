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
use ws::WsState;
use ws::connection_manager::ConnectionManager;
use ws::terminal_registry::TerminalRegistry;

pub async fn run_hub() -> anyhow::Result<()> {
    // Load configuration
    let config = Configuration::create().await?;

    info!(
        port = config.listen_port,
        host = %config.listen_host,
        public_url = %config.public_url,
        telegram = config.telegram_enabled,
        "starting hub"
    );

    // Create store
    let db_path_str = config.db_path.to_string_lossy().to_string();
    let store = Arc::new(Store::new(&db_path_str)?);

    // Get/create JWT secret
    let jwt_secret = config::jwt_secret::get_or_create_jwt_secret(&config.data_dir)?;

    // Get/create VAPID keys (optional — push notifications disabled if unavailable)
    let vapid_keys = config::vapid_keys::get_or_create_vapid_keys(&config.data_dir).ok();
    let vapid_public_key = vapid_keys.as_ref().map(|k| k.public_key.clone());

    // Create connection manager
    let conn_mgr = Arc::new(ConnectionManager::new());

    // Create SyncEngine
    let sync_engine = Arc::new(Mutex::new(SyncEngine::new(store.clone(), conn_mgr.clone())));

    // Set up notification channels
    let mut notification_channels: Vec<Arc<dyn NotificationChannel>> = Vec::new();

    if let Some(ref keys) = vapid_keys {
        let vapid_subject =
            std::env::var("VAPID_SUBJECT").unwrap_or_else(|_| "mailto:admin@localhost".into());
        let push_service = Arc::new(PushService::new(keys.clone(), vapid_subject, store.clone()));
        let push_channel = Arc::new(PushNotificationChannel::new(
            push_service,
            sync_engine.clone(),
            config.public_url.clone(),
        ));
        notification_channels.push(push_channel);
    }

    // Initialize Telegram bot (optional)
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

    // Start notification hub
    let notification_hub = Arc::new(NotificationHub::new(
        notification_channels,
        5_000, // ready cooldown ms
        500,   // permission debounce ms
    ));
    notification_hub.clone().start(sync_engine.clone()).await;

    // Build AppState for HTTP routes
    let app_state = AppState {
        jwt_secret: jwt_secret.clone(),
        cli_api_token: config.cli_api_token.clone(),
        sync_engine: sync_engine.clone(),
        store: store.clone(),
        vapid_public_key,
        telegram_bot_token: config.telegram_bot_token.clone(),
        data_dir: config.data_dir.clone(),
        cors_origins: config.cors_origins.clone(),
    };

    // Build WsState for WebSocket handlers
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

    // Build combined router
    let terminal_registry_for_idle = ws_state.terminal_registry.clone();
    let web_router = web::build_router(app_state);
    let ws_router = ws::ws_router(ws_state);
    let app = web_router.merge(ws_router);

    // Start periodic expiration timer
    let sync_for_expire = sync_engine.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            sync_for_expire.lock().await.expire_inactive();
        }
    });

    // Start terminal idle timeout checker
    let conn_mgr_for_idle = conn_mgr.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let idle_ids = terminal_registry_for_idle.read().await.collect_idle();
            if idle_ids.is_empty() {
                continue;
            }
            let mut reg = terminal_registry_for_idle.write().await;
            for terminal_id in idle_ids {
                if let Some(entry) = reg.remove(&terminal_id) {
                    // Notify terminal webapp socket
                    let err_msg = serde_json::json!({
                        "event": "terminal:error",
                        "data": {
                            "terminalId": entry.terminal_id,
                            "message": "Terminal closed due to inactivity."
                        }
                    });
                    conn_mgr_for_idle.send_to(&entry.socket_id, &err_msg.to_string()).await;

                    // Notify CLI socket
                    let close_msg = serde_json::json!({
                        "event": "terminal:close",
                        "data": {
                            "sessionId": entry.session_id,
                            "terminalId": entry.terminal_id
                        }
                    });
                    conn_mgr_for_idle.send_to(&entry.cli_socket_id, &close_msg.to_string()).await;
                }
            }
        }
    });

    // Start SSE heartbeat timer (lazy: only sends when connections exist)
    let sync_for_heartbeat = sync_engine.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let mut engine = sync_for_heartbeat.lock().await;
            if engine.sse_manager().connection_count() > 0 {
                engine.sse_manager_mut().send_heartbeats();
            }
        }
    });

    // Display startup info
    if config.cli_api_token_is_new {
        info!(token = %config.cli_api_token, "generated new CLI API token");
    }

    // Bind and serve
    let addr = format!("{}:{}", config.listen_host, config.listen_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "listening");

    // Start Telegram bot (after HTTP server is ready)
    if let Some(ref bot) = happy_bot {
        bot.start();
    }

    info!(url = %config.public_url, "hub ready");

    // Use a Notify so we can trigger graceful shutdown from outside
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let shutdown_notify_srv = shutdown_notify.clone();

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_notify_srv.notified().await;
            })
            .await
    });

    // Wait for OS signal
    shutdown_signal().await;

    // Actively close all WebSocket connections
    info!("closing all WebSocket connections");
    conn_mgr.close_all().await;

    // Tell axum to stop accepting new connections
    shutdown_notify.notify_one();

    // Give axum up to 5s to finish, then force abort
    if tokio::time::timeout(std::time::Duration::from_secs(5), server_task).await.is_err() {
        info!("graceful shutdown timed out, forcing exit");
    }

    // Graceful shutdown: stop bot
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
