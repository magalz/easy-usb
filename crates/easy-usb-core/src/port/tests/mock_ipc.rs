use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::ipc::{Handler, IpcError, IpcServer};

#[derive(Clone)]
pub struct MockIpc {
    routes: std::sync::Arc<Mutex<HashMap<String, Handler>>>,
    serving: std::sync::Arc<Mutex<bool>>,
}

impl MockIpc {
    pub fn new() -> Self {
        MockIpc {
            routes: std::sync::Arc::new(Mutex::new(HashMap::new())),
            serving: std::sync::Arc::new(Mutex::new(false)),
        }
    }

    pub fn call_route(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value, IpcError> {
        let routes = self.routes.lock().unwrap();
        let handler = routes
            .get(path)
            .ok_or_else(|| IpcError::RouteNotFound { path: path.to_string() })?
            .clone();
        drop(routes);

        Ok(futures::executor::block_on(handler(body)))
    }

    pub fn routes(&self) -> HashMap<String, Handler> {
        self.routes.lock().unwrap().clone()
    }
}

#[async_trait]
impl IpcServer for MockIpc {
    type Error = IpcError;

    async fn serve(&mut self, routes: HashMap<String, Handler>) -> Result<(), Self::Error> {
        {
            let mut serving = self.serving.lock().unwrap();
            if *serving {
                return Err(IpcError::AlreadyServing);
            }
            *serving = true;
        }
        let mut self_routes = self.routes.lock().unwrap();
        *self_routes = routes;
        tracing::info!("ipc server serving {} routes", self_routes.len());
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), Self::Error> {
        let mut serving = self.serving.lock().unwrap();
        if !*serving {
            return Ok(());
        }
        *serving = false;
        let mut routes = self.routes.lock().unwrap();
        routes.clear();
        tracing::info!("ipc server shut down");
        Ok(())
    }
}
