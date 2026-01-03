use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

/// A request to be proxied through the tunnel
pub struct ProxyRequest {
    pub request_bytes: Bytes,
    pub response_tx: oneshot::Sender<Result<Bytes, ProxyError>>,
}

#[derive(Debug)]
pub enum ProxyError {
    StreamOpenFailed,
    WriteFailed,
    ReadFailed,
    Timeout,
    ConnectionClosed,
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::StreamOpenFailed => write!(f, "Failed to open stream"),
            ProxyError::WriteFailed => write!(f, "Failed to write to stream"),
            ProxyError::ReadFailed => write!(f, "Failed to read from stream"),
            ProxyError::Timeout => write!(f, "Timeout"),
            ProxyError::ConnectionClosed => write!(f, "Connection closed"),
        }
    }
}

#[allow(dead_code)]
pub struct Tunnel {
    pub subdomain: String,
    pub token: String,
    pub request_tx: mpsc::Sender<ProxyRequest>,
    pub created_at: Instant,
    pub request_count: AtomicU64,
    last_activity: RwLock<Instant>,
}

impl Tunnel {
    pub fn new(
        subdomain: String,
        token: String,
        request_tx: mpsc::Sender<ProxyRequest>,
    ) -> Self {
        let now = Instant::now();
        Self {
            subdomain,
            token,
            request_tx,
            created_at: now,
            request_count: AtomicU64::new(0),
            last_activity: RwLock::new(now),
        }
    }

    pub fn increment_requests(&self) -> u64 {
        self.touch();
        self.request_count.fetch_add(1, Ordering::Relaxed)
    }

    /// Update the last activity timestamp
    pub fn touch(&self) {
        if let Ok(mut last) = self.last_activity.write() {
            *last = Instant::now();
        }
    }

    /// Get the last activity time
    pub fn last_activity(&self) -> Instant {
        self.last_activity.read().map(|t| *t).unwrap_or(self.created_at)
    }

    /// Check if the tunnel has been idle for longer than the given duration
    pub fn is_idle(&self, timeout: std::time::Duration) -> bool {
        self.last_activity().elapsed() > timeout
    }

    pub async fn proxy(&self, request_bytes: Bytes) -> Result<Bytes, ProxyError> {
        let (response_tx, response_rx) = oneshot::channel();
        let request = ProxyRequest {
            request_bytes,
            response_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| ProxyError::ConnectionClosed)?;

        response_rx.await.map_err(|_| ProxyError::ConnectionClosed)?
    }
}
