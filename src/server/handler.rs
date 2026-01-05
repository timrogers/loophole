use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use futures::StreamExt;
use crate::proto::{ClientMessage, ErrorCode, ServerMessage};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use yamux::{Connection, Mode};

use super::compat::Compat;
use super::registry::Registry;
use super::router::ServerState;
use super::tunnel::{ProxyError, ProxyRequest, Tunnel};

pub async fn handle_websocket(
    mut socket: WebSocket,
    state: Arc<ServerState>,
    addr: SocketAddr,
) -> Result<()> {
    // Wait for Register message
    let (token, subdomain) = match wait_for_registration(&mut socket).await? {
        Some((t, s)) => (t, s),
        None => return Ok(()),
    };

    debug!("Registration request: subdomain={}, from={}", subdomain, addr);

    // Validate token
    if state.config.validate_token(&token).is_none() {
        warn!("Invalid token from {}", addr);
        send_error(&mut socket, ErrorCode::InvalidToken, "Invalid token").await;
        return Ok(());
    }

    // Validate subdomain
    if let Err(e) = Registry::validate_subdomain(&subdomain) {
        warn!("Invalid subdomain '{}': {}", subdomain, e);
        send_error(&mut socket, ErrorCode::SubdomainInvalid, e.to_string()).await;
        return Ok(());
    }

    // Determine URL based on HTTPS availability
    let full_domain = format!("{}.{}", subdomain, state.config.server.domain);
    let (url, cert_ready) = if state.config.acme.is_some() {
        // HTTPS mode
        let https_port = state.config.server.https_port;
        let url = if https_port == 443 {
            format!("https://{}", full_domain)
        } else {
            format!("https://{}:{}", full_domain, https_port)
        };
        
        // Check if certificate exists
        let cert_ready = state.cert_manager
            .as_ref()
            .map(|cm| cm.has_cert(&full_domain))
            .unwrap_or(false);
        
        (url, cert_ready)
    } else {
        // HTTP mode
        let http_port = state.config.server.http_port;
        let url = if http_port == 80 {
            format!("http://{}", full_domain)
        } else {
            format!("http://{}:{}", full_domain, http_port)
        };
        (url, true) // No cert needed for HTTP
    };

    // Send success response first
    let response = ServerMessage::Registered {
        subdomain: subdomain.clone(),
        url: url.clone(),
    };
    if socket
        .send(Message::Text(response.to_json().unwrap().into()))
        .await
        .is_err()
    {
        return Ok(());
    }

    info!("Tunnel registered: {} -> {}", subdomain, url);

    // If HTTPS is enabled and cert doesn't exist, request it
    if state.config.acme.is_some() {
        if !cert_ready {
            // Send certificate status (not ready)
            let cert_status = ServerMessage::CertificateStatus { ready: false };
            let _ = socket.send(Message::Text(cert_status.to_json().unwrap().into())).await;
            
            // Request certificate in background
            if let Some(ref cert_manager) = state.cert_manager {
                let cm = cert_manager.clone();
                let domain = full_domain.clone();
                
                tokio::spawn(async move {
                    match cm.request_cert(&domain).await {
                        Ok(()) => info!("Certificate ready for {}", domain),
                        Err(e) => error!("Failed to get certificate for {}: {}", domain, e),
                    }
                });
            }
        } else {
            // Send certificate status (ready)
            let cert_status = ServerMessage::CertificateStatus { ready: true };
            let _ = socket.send(Message::Text(cert_status.to_json().unwrap().into())).await;
        }
    }

    // Create channel for proxy requests
    let (request_tx, mut request_rx) = mpsc::channel::<ProxyRequest>(32);

    // Create tunnel with channel sender
    let tunnel = Arc::new(Tunnel::new(subdomain.clone(), token, request_tx));

    // Register in registry
    if let Err(e) = state.registry.register(&subdomain, tunnel.clone()) {
        error!("Failed to register tunnel: {}", e);
        return Ok(());
    }

    // Create yamux connection
    let config = yamux::Config::default();
    let compat_ws = Compat::new(socket);
    let mut connection = Connection::new(compat_ws, config, Mode::Server);

    // Run the connection handler loop
    loop {
        tokio::select! {
            // Handle proxy requests from the channel
            Some(request) = request_rx.recv() => {
                debug!("Received proxy request for {} bytes", request.request_bytes.len());
                
                // Open a new outbound stream
                let stream_result = std::future::poll_fn(|cx| connection.poll_new_outbound(cx)).await;
                
                match stream_result {
                    Ok(mut stream) => {
                        // Spawn a task to handle this stream
                        let request_bytes = request.request_bytes;
                        let response_tx = request.response_tx;
                        
                        tokio::spawn(async move {
                            let result = handle_proxy_stream(&mut stream, request_bytes).await;
                            let _ = response_tx.send(result);
                        });
                    }
                    Err(e) => {
                        error!("Failed to open stream: {}", e);
                        let _ = request.response_tx.send(Err(ProxyError::StreamOpenFailed));
                    }
                }
            }
            
            // Poll the connection to drive yamux
            poll_result = std::future::poll_fn(|cx| connection.poll_next_inbound(cx)) => {
                match poll_result {
                    Some(Ok(_stream)) => {
                        // We don't expect inbound streams from client
                        debug!("Unexpected inbound stream from client");
                    }
                    Some(Err(e)) => {
                        // Connection errors are expected when clients disconnect
                        let err_str = e.to_string();
                        if err_str.contains("Connection reset") || err_str.contains("closed") {
                            debug!("Tunnel {} connection closed: {}", subdomain, e);
                        } else {
                            warn!("Tunnel {} connection error: {}", subdomain, e);
                        }
                        break;
                    }
                    None => {
                        info!("Tunnel {} disconnected", subdomain);
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    state.registry.deregister(&subdomain);
    info!("Tunnel {} deregistered", subdomain);

    Ok(())
}

async fn handle_proxy_stream(
    stream: &mut yamux::Stream,
    request_bytes: Bytes,
) -> Result<Bytes, ProxyError> {
    // Write request
    stream
        .write_all(&request_bytes)
        .await
        .map_err(|_| ProxyError::WriteFailed)?;
    
    // Flush to ensure data is sent
    stream.flush().await.map_err(|_| ProxyError::WriteFailed)?;

    // Read response with timeout
    let mut response_bytes = Vec::new();
    let read_result = tokio::time::timeout(
        Duration::from_secs(30),
        stream.read_to_end(&mut response_bytes),
    )
    .await;

    match read_result {
        Ok(Ok(_)) => Ok(Bytes::from(response_bytes)),
        Ok(Err(_)) => Err(ProxyError::ReadFailed),
        Err(_) => Err(ProxyError::Timeout),
    }
}

async fn wait_for_registration(socket: &mut WebSocket) -> Result<Option<(String, String)>> {
    // Set a timeout for registration
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next()).await;

    match result {
        Ok(Some(Ok(Message::Text(text)))) => {
            match ClientMessage::from_json(&text) {
                Ok(ClientMessage::Register { token, subdomain }) => {
                    Ok(Some((token, subdomain)))
                }
                Ok(_) => {
                    warn!("Expected Register message, got something else");
                    send_error(socket, ErrorCode::InternalError, "Expected Register message").await;
                    Ok(None)
                }
                Err(e) => {
                    warn!("Failed to parse client message: {}", e);
                    send_error(socket, ErrorCode::InternalError, "Invalid message format").await;
                    Ok(None)
                }
            }
        }
        Ok(Some(Ok(_))) => {
            warn!("Expected text message");
            send_error(socket, ErrorCode::InternalError, "Expected text message").await;
            Ok(None)
        }
        Ok(Some(Err(e))) => {
            error!("WebSocket error: {}", e);
            Ok(None)
        }
        Ok(None) => {
            debug!("WebSocket closed before registration");
            Ok(None)
        }
        Err(_) => {
            warn!("Registration timeout");
            send_error(socket, ErrorCode::InternalError, "Registration timeout").await;
            Ok(None)
        }
    }
}

async fn send_error(socket: &mut WebSocket, code: ErrorCode, message: impl Into<String>) {
    let msg = ServerMessage::error(code, message);
    if let Ok(json) = msg.to_json() {
        let _ = socket.send(Message::Text(json.into())).await;
    }
}
