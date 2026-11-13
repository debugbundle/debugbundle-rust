//! Explicit custom delivery boundary. Callbacks execute on a bounded worker.
use serde_json::Value;
use std::time::Duration;

pub struct TransportResponse {
    pub status: u16,
    pub body: Option<Value>,
    pub retry_after: Option<Duration>,
}
#[derive(Debug)]
pub struct TransportFailure;

pub trait Transport: Send + Sync + 'static {
    fn send(&self, body: &[u8], timeout: Duration) -> Result<TransportResponse, TransportFailure>;
    /// Only an explicitly configured legacy/custom transport may waive a body.
    fn requires_acknowledgement(&self) -> bool {
        true
    }
}
