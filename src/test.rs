use anyhow::Result;
use colored::Colorize;

use crate::client_config::ClientConfig;

/// Check connection to server by attempting to register and immediately disconnect
pub async fn check_connection(server: &str, token: &str) -> Result<()> {
    use crate::proto::{ClientMessage, ServerMessage};
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    // Build WebSocket URL
    let ws_url = if server.starts_with("https://") {
        server.replace("https://", "wss://")
    } else if server.starts_with("http://") {
        server.replace("http://", "ws://")
    } else {
        format!("ws://{}", server)
    };
    let ws_url = format!("{}/_tunnel/connect", ws_url);

    // Connect
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to server: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    // Send a registration with a test subdomain
    let test_subdomain = format!("test-{}", std::process::id());
    let register_msg = ClientMessage::Register {
        token: token.to_string(),
        subdomain: test_subdomain,
    };
    let json = register_msg.to_json()?;
    write.send(Message::Text(json.into())).await?;

    // Wait for response
    let response = read
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("Connection closed before response"))?
        .map_err(|e| anyhow::anyhow!("WebSocket error: {}", e))?;

    let response_text = match response {
        Message::Text(t) => t.to_string(),
        _ => anyhow::bail!("Unexpected response from server"),
    };

    let server_msg = ServerMessage::from_json(&response_text)?;
    match server_msg {
        ServerMessage::Registered { .. } => {
            // Success! Close connection gracefully
            let _ = write.send(Message::Close(None)).await;
            Ok(())
        }
        ServerMessage::Error { code, message } => {
            use crate::proto::ErrorCode;
            match code {
                ErrorCode::InvalidToken => Err(anyhow::anyhow!("Invalid token")),
                ErrorCode::SubdomainTaken => {
                    // This actually means auth worked, subdomain just taken
                    let _ = write.send(Message::Close(None)).await;
                    Ok(())
                }
                _ => Err(anyhow::anyhow!("Server error: {}", message)),
            }
        }
        _ => Err(anyhow::anyhow!("Unexpected response from server")),
    }
}

pub async fn run(server: Option<String>, token: Option<String>) -> Result<()> {
    // Load from config if not provided
    let (server, token) = match (server, token) {
        (Some(s), Some(t)) => (s, t),
        (s, t) => {
            let config = ClientConfig::load()?
                .ok_or_else(|| anyhow::anyhow!("Not logged in. Run 'loophole login' first."))?;
            (s.unwrap_or(config.server), t.unwrap_or(config.token))
        }
    };

    println!("{} Testing connection to {}...", "→".cyan(), server);

    match check_connection(&server, &token).await {
        Ok(()) => {
            println!("{} Connection successful!", "✓".green());
            println!("{} Token is valid", "✓".green());
            println!("{} Server is accepting connections", "✓".green());
            Ok(())
        }
        Err(e) => {
            println!("{} Connection failed: {}", "✗".red(), e);
            Err(e)
        }
    }
}
