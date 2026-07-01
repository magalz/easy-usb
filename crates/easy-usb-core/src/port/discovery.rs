use async_trait::async_trait;
use thiserror::Error;

use crate::port::types::{ServerInfo, ServiceInfo};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiscoveryError {
    #[error("advertise failed: {0}")]
    AdvertiseFailed(String),
    #[error("scan failed: {0}")]
    ScanFailed(String),
    #[error("withdraw failed: {0}")]
    WithdrawFailed(String),
    #[error("not initialized: {0}")]
    NotInitialized(String),
}

#[async_trait]
pub trait DiscoveryService {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn advertise(&self, info: &ServiceInfo) -> Result<(), Self::Error>;
    async fn scan(&self) -> Result<Vec<ServerInfo>, Self::Error>;
    async fn withdraw(&self) -> Result<(), Self::Error>;
}
