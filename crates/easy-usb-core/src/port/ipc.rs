use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

pub type Handler =
    Arc<dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IpcError {
    #[error("bind failed: {0}")]
    BindFailed(String),
    #[error("route not found: {path}")]
    RouteNotFound { path: String },
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),
    #[error("already serving")]
    AlreadyServing,
}

#[async_trait]
pub trait IpcServer {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn serve(&mut self, routes: HashMap<String, Handler>) -> Result<(), Self::Error>;
    async fn shutdown(&self) -> Result<(), Self::Error>;
}
