pub mod types;

use std::future::Future;

use serde_json::Value;

/// Trait for registering RPC handlers.
///
/// Implementors scope the method name (e.g. with a session or machine ID)
/// and forward the handler to the underlying WebSocket client.
pub trait RpcRegistry {
    fn register<F, Fut>(&self, method: &str, handler: F) -> impl Future<Output = ()> + Send
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Value> + Send + 'static;
}
