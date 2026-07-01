use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::ipc::Handler;
use crate::port::session::{SessionSignal, SessionSignalError};
use crate::port::types::SignalMode;

#[derive(Clone)]
pub struct MockSessionSignal {
    on_start_handler: std::sync::Arc<Mutex<Option<(Handler, SignalMode)>>>,
    on_stop_handler: std::sync::Arc<Mutex<Option<Handler>>>,
    start_count: std::sync::Arc<Mutex<u32>>,
    stop_count: std::sync::Arc<Mutex<u32>>,
}

impl MockSessionSignal {
    pub fn new() -> Self {
        MockSessionSignal {
            on_start_handler: std::sync::Arc::new(Mutex::new(None)),
            on_stop_handler: std::sync::Arc::new(Mutex::new(None)),
            start_count: std::sync::Arc::new(Mutex::new(0)),
            stop_count: std::sync::Arc::new(Mutex::new(0)),
        }
    }

    pub fn simulate_start(&self, body: serde_json::Value) {
        let handler = {
            let guard = self.on_start_handler.lock().unwrap();
            guard.clone()
        };
        if let Some((handler, _mode)) = handler {
            *self.start_count.lock().unwrap() += 1;
            futures::executor::block_on(handler(body));
        }
    }

    pub fn simulate_stop(&self, body: serde_json::Value) {
        let handler = {
            let guard = self.on_stop_handler.lock().unwrap();
            guard.clone()
        };
        if let Some(handler) = handler {
            *self.stop_count.lock().unwrap() += 1;
            futures::executor::block_on(handler(body));
        }
    }

    pub fn start_count(&self) -> u32 {
        *self.start_count.lock().unwrap()
    }

    pub fn stop_count(&self) -> u32 {
        *self.stop_count.lock().unwrap()
    }
}

#[async_trait]
impl SessionSignal for MockSessionSignal {
    type Error = SessionSignalError;

    async fn on_start(&self, handler: Handler, mode: SignalMode) -> Result<(), Self::Error> {
        let mut guard = self.on_start_handler.lock().unwrap();
        if guard.is_some() {
            return Err(SessionSignalError::AlreadyRegistered);
        }
        *guard = Some((handler, mode));
        Ok(())
    }

    async fn on_stop(&self, handler: Handler) -> Result<(), Self::Error> {
        let mut guard = self.on_stop_handler.lock().unwrap();
        if guard.is_some() {
            return Err(SessionSignalError::AlreadyRegistered);
        }
        *guard = Some(handler);
        Ok(())
    }
}
