pub mod types;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Async RPC handler function type
pub type RpcHandlerFn = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync,
>;

/// Manages RPC handlers with scoped method names.
pub struct RpcHandlerManager {
    scope_prefix: String,
    handlers: Arc<RwLock<HashMap<String, RpcHandlerFn>>>,
}

impl RpcHandlerManager {
    pub fn new(scope_prefix: &str) -> Self {
        Self {
            scope_prefix: scope_prefix.to_string(),
            handlers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a handler for a method. The method name will be prefixed with scope.
    pub async fn register<F, Fut>(&self, method: &str, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Value> + Send + 'static,
    {
        let scoped = format!("{}:{}", self.scope_prefix, method);
        debug!(method = %scoped, "registering RPC handler");
        self.handlers.write().await.insert(
            scoped,
            Arc::new(move |params| Box::pin(handler(params))),
        );
    }

    /// Unregister a handler.
    pub async fn unregister(&self, method: &str) {
        let scoped = format!("{}:{}", self.scope_prefix, method);
        self.handlers.write().await.remove(&scoped);
    }

    /// Handle an incoming RPC request. Returns the response value.
    pub async fn handle_request(&self, method: &str, params: Value) -> Value {
        let handlers = self.handlers.read().await;
        if let Some(handler) = handlers.get(method) {
            let handler = handler.clone();
            drop(handlers); // Release lock before calling handler
            handler(params).await
        } else {
            warn!(method = %method, "no RPC handler registered");
            serde_json::json!({"error": format!("unknown method: {method}")})
        }
    }

    /// Get all registered method names (for rpc-register events).
    pub async fn methods(&self) -> Vec<String> {
        self.handlers.read().await.keys().cloned().collect()
    }

    /// Get the scope prefix.
    pub fn scope_prefix(&self) -> &str {
        &self.scope_prefix
    }
}
