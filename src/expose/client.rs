use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use crate::proto::{ClientMessage, ErrorCode, ServerMessage};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info};

pub struct TunnelClient {
    pub server: String,  // Full URL with scheme (e.g., https://tunnel.example.com)
    pub token: String,
    pub subdomain: String,
    pub control_path: String,
}

impl TunnelClient {
    pub fn new(server: String, token: String, subdomain: String) -> Self {
        Self {
            server,
            token,
            subdomain,
            control_path: "/_tunnel/connect".to_string(),
        }
    }

    pub async fn connect(&self) -> Result<TunnelConnection> {
        // Convert HTTP(S) URL to WS(S) URL
        let ws_url = if self.server.starts_with("https://") {
            self.server.replace("https://", "wss://")
        } else if self.server.starts_with("http://") {
            self.server.replace("http://", "ws://")
        } else {
            // Legacy: no scheme provided, default to wss://
            format!("wss://{}", self.server)
        };
        let ws_url = format!("{}{}", ws_url, self.control_path);
        
        info!("Connecting to {}", ws_url);
        
        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .context("Failed to connect to server")?;

        debug!("WebSocket connection established");

        let (mut write, mut read) = ws_stream.split();

        // Send registration message
        let register_msg = ClientMessage::Register {
            token: self.token.clone(),
            subdomain: self.subdomain.clone(),
        };
        let json = register_msg.to_json()?;
        write.send(Message::Text(json.into())).await?;
        debug!("Sent registration request");

        // Wait for response
        let response = read
            .next()
            .await
            .context("Connection closed before registration response")?
            .context("WebSocket error")?;

        let response_text = match response {
            Message::Text(t) => t.to_string(),
            _ => anyhow::bail!("Expected text message"),
        };

        let server_msg = ServerMessage::from_json(&response_text)?;
        match server_msg {
            ServerMessage::Registered { subdomain, url } => {
                info!("Tunnel registered!");
                info!("Subdomain: {}", subdomain);
                info!("URL: {}", url);
                Ok(TunnelConnection {
                    write,
                    read,
                    subdomain,
                    url,
                    cert_ready: None, // Will be determined by CertificateStatus message
                })
            }
            ServerMessage::Error { code, message } => {
                error!("Registration failed: {:?} - {}", code, message);
                match code {
                    ErrorCode::InvalidToken => anyhow::bail!("Invalid token"),
                    ErrorCode::SubdomainTaken => anyhow::bail!("Subdomain already taken"),
                    ErrorCode::SubdomainInvalid => anyhow::bail!("Invalid subdomain: {}", message),
                    ErrorCode::TunnelLimitReached => anyhow::bail!("Tunnel limit reached"),
                    ErrorCode::InternalError => anyhow::bail!("Server error: {}", message),
                }
            }
            _ => anyhow::bail!("Unexpected server response"),
        }
    }

    /// Wait for the server to send CertificateStatus message.
    /// Returns true if cert is ready, false if not ready (still provisioning).
    /// Returns None if no certificate status was sent (e.g., ACME not configured).
    pub async fn wait_for_cert_status(read: &mut futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >) -> Option<bool> {
        use tokio::time::{timeout, Duration};
        
        // Wait up to 1 second for certificate status message
        // If no message arrives, assume no ACME configured
        let result = timeout(Duration::from_secs(1), read.next()).await;
        
        match result {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(msg) = ServerMessage::from_json(&text) {
                    if let ServerMessage::CertificateStatus { ready } = msg {
                        return Some(ready);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Wait for the certificate to become ready by polling for CertificateStatus messages.
    /// Returns when cert is ready or timeout is reached.
    pub async fn wait_for_cert_ready(read: &mut futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >, timeout_secs: u64) -> bool {
        use tokio::time::{timeout, Duration};
        
        let deadline = Duration::from_secs(timeout_secs);
        let result = timeout(deadline, async {
            loop {
                match read.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(msg) = ServerMessage::from_json(&text) {
                            if let ServerMessage::CertificateStatus { ready } = msg {
                                if ready {
                                    return true;
                                }
                                // Not ready yet, keep waiting
                            }
                        }
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                        // Ignore ping/pong, keep waiting
                    }
                    _ => {
                        // Connection closed or error
                        return false;
                    }
                }
            }
        }).await;
        
        result.unwrap_or(false)
    }
}

#[allow(dead_code)]
pub struct TunnelConnection {
    pub write: futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    pub read: futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    pub subdomain: String,
    pub url: String,
    pub cert_ready: Option<bool>,
}
