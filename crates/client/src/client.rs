use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use proto::{ClientMessage, ErrorCode, ServerMessage};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info};

pub struct TunnelClient {
    pub server: String,
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
        let url = format!("ws://{}{}", self.server, self.control_path);
        info!("Connecting to {}", url);

        let (ws_stream, _) = connect_async(&url)
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
}
