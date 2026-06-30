use async_trait::async_trait;
use thiserror::Error;

use crate::port::ipc::Handler;
use crate::port::types::SignalMode;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionSignalError {
    #[error("signal failed: {0}")]
    SignalFailed(String),
    #[error("already registered")]
    AlreadyRegistered,
    #[error("not registered")]
    NotRegistered,
}

#[async_trait]
pub trait SessionSignal {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn on_start(&self, handler: Handler, mode: SignalMode) -> Result<(), Self::Error>;
    async fn on_stop(&self, handler: Handler) -> Result<(), Self::Error>;
}
