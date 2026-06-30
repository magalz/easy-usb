use async_trait::async_trait;
use futures::Stream;
use thiserror::Error;

use crate::port::types::{UsbDevice, UsbEvent};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UsbHostError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("device not found: {busid}")]
    DeviceNotFound { busid: String },
    #[error("device already in use: {busid}")]
    DeviceInUse { busid: String },
    #[error("platform error: {0}")]
    PlatformError(String),
}

#[async_trait]
pub trait UsbHost {
    type Error: std::error::Error + Send + Sync + 'static;
    type WatchStream: Stream<Item = UsbEvent> + Send + Unpin + 'static;

    async fn list(&self) -> Result<Vec<UsbDevice>, Self::Error>;
    async fn bind(&mut self, busid: &str) -> Result<(), Self::Error>;
    async fn unbind(&mut self, busid: &str) -> Result<(), Self::Error>;
    fn watch(&mut self) -> Self::WatchStream;
}
