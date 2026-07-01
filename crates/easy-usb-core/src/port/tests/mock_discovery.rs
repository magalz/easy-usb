use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::discovery::{DiscoveryError, DiscoveryService};
use crate::port::types::{ServerInfo, ServiceInfo};

#[derive(Clone)]
pub struct MockDiscovery {
    scan_results: std::sync::Arc<Mutex<Vec<ServerInfo>>>,
    result: std::sync::Arc<Mutex<Result<(), String>>>,
}

impl MockDiscovery {
    pub fn new(server_list: Vec<ServerInfo>) -> Self {
        MockDiscovery {
            scan_results: std::sync::Arc::new(Mutex::new(server_list)),
            result: std::sync::Arc::new(Mutex::new(Ok(()))),
        }
    }

    #[allow(dead_code)]
    pub fn set_scan_result(&self, servers: Vec<ServerInfo>) {
        let mut results = self.scan_results.lock().unwrap();
        *results = servers;
    }

    pub fn set_result(&self, res: Result<(), String>) {
        let mut result = self.result.lock().unwrap();
        *result = res;
    }
}

#[async_trait]
impl DiscoveryService for MockDiscovery {
    type Error = DiscoveryError;

    async fn advertise(&self, _info: &ServiceInfo) -> Result<(), Self::Error> {
        tracing::info!("advertise called");
        let result = self.result.lock().unwrap();
        match &*result {
            Ok(()) => Ok(()),
            Err(msg) => Err(DiscoveryError::AdvertiseFailed(msg.clone())),
        }
    }

    async fn scan(&self) -> Result<Vec<ServerInfo>, Self::Error> {
        let results = self.scan_results.lock().unwrap();
        Ok(results.clone())
    }

    async fn withdraw(&self) -> Result<(), Self::Error> {
        let result = self.result.lock().unwrap();
        match &*result {
            Ok(()) => Ok(()),
            Err(msg) => Err(DiscoveryError::WithdrawFailed(msg.clone())),
        }
    }
}
