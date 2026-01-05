use anyhow::Result;
use axum::body::Body;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use http_body_util::BodyExt;
use hyper::StatusCode;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, warn};

use super::tunnel::Tunnel;

pub async fn proxy_request(
    tunnel: Arc<Tunnel>,
    req: hyper::Request<axum::body::Body>,
    client_ip: std::net::IpAddr,
    is_https: bool,
) -> Result<Response> {
    let request_id = uuid::Uuid::new_v4().to_string();
    tunnel.increment_requests();

    // Get a yamux stream from the tunnel
    let mut stream = match tunnel.get_stream().await {
        Ok(s) => s,
        Err(e) => {
            error!(request_id = %request_id, "Failed to get tunnel stream: {}", e);
            return Ok(bad_gateway("Failed to connect to tunnel"));
        }
    };

    // Build and send request headers
    let (parts, body) = req.into_parts();
    
    let mut header_bytes = Vec::new();
    header_bytes.extend_from_slice(
        format!(
            "{} {} HTTP/1.1\r\n",
            parts.method,
            parts.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
        )
        .as_bytes(),
    );

    // Add headers (skip hop-by-hop headers)
    for (name, value) in &parts.headers {
        if !is_hop_by_hop_header(name.as_str()) {
            header_bytes.extend_from_slice(format!("{}: ", name).as_bytes());
            header_bytes.extend_from_slice(value.as_bytes());
            header_bytes.extend_from_slice(b"\r\n");
        }
    }

    // Add forwarded headers
    let proto = if is_https { "https" } else { "http" };
    header_bytes.extend_from_slice(format!("X-Forwarded-For: {}\r\n", client_ip).as_bytes());
    header_bytes.extend_from_slice(format!("X-Forwarded-Proto: {}\r\n", proto).as_bytes());
    header_bytes.extend_from_slice(format!("X-Request-ID: {}\r\n", request_id).as_bytes());
    header_bytes.extend_from_slice(b"\r\n");

    // Write headers to tunnel
    if let Err(e) = stream.write_all(&header_bytes).await {
        error!(request_id = %request_id, "Failed to write headers to tunnel: {}", e);
        return Ok(bad_gateway("Failed to send request to tunnel"));
    }

    // Stream request body to tunnel
    let mut body_stream = body;
    while let Some(chunk) = body_stream.frame().await {
        match chunk {
            Ok(frame) => {
                if let Ok(data) = frame.into_data() {
                    if let Err(e) = stream.write_all(&data).await {
                        error!(request_id = %request_id, "Failed to write body to tunnel: {}", e);
                        return Ok(bad_gateway("Failed to send request body to tunnel"));
                    }
                }
            }
            Err(e) => {
                error!(request_id = %request_id, "Failed to read request body: {}", e);
                return Ok(bad_gateway("Failed to read request body"));
            }
        }
    }

    // Flush to ensure all data is sent
    if let Err(e) = stream.flush().await {
        error!(request_id = %request_id, "Failed to flush tunnel stream: {}", e);
        return Ok(bad_gateway("Failed to send request to tunnel"));
    }

    debug!(request_id = %request_id, "Request sent to tunnel, reading response");

    // Read response headers from tunnel with timeout
    let mut header_buf = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end;
    
    let header_read_timeout = std::time::Duration::from_secs(30);
    let header_read_start = std::time::Instant::now();
    
    loop {
        // Check timeout
        if header_read_start.elapsed() > header_read_timeout {
            warn!(request_id = %request_id, "Timeout waiting for response headers");
            return Ok(gateway_timeout("Timeout waiting for response"));
        }
        
        // Use a short timeout for each read to allow checking overall timeout
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read(&mut buf)
        ).await;
        
        match read_result {
            Ok(Ok(0)) => {
                warn!(request_id = %request_id, "Tunnel closed before response headers");
                return Ok(bad_gateway("Tunnel closed unexpectedly"));
            }
            Ok(Ok(n)) => {
                header_buf.extend_from_slice(&buf[..n]);
                if let Some(pos) = find_header_end(&header_buf) {
                    header_end = pos;
                    break;
                }
                if header_buf.len() > 65536 {
                    warn!(request_id = %request_id, "Response headers too large");
                    return Ok(bad_gateway("Response headers too large"));
                }
            }
            Ok(Err(e)) => {
                error!(request_id = %request_id, "Failed to read response from tunnel: {}", e);
                return Ok(bad_gateway("Failed to read response from tunnel"));
            }
            Err(_) => {
                // Individual read timeout - continue to check overall timeout
                continue;
            }
        }
    }

    // Parse response headers
    let header_bytes = &header_buf[..header_end];
    let initial_body = header_buf[header_end + 4..].to_vec(); // Data after \r\n\r\n

    let header_str = match std::str::from_utf8(header_bytes) {
        Ok(s) => s,
        Err(_) => {
            warn!(request_id = %request_id, "Invalid UTF-8 in response headers");
            return Ok(bad_gateway("Invalid response from backend"));
        }
    };

    let mut lines = header_str.lines();
    let status_line = lines.next().unwrap_or("HTTP/1.1 502 Bad Gateway");
    let status_parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    let status_code = status_parts
        .get(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(502);

    let mut builder = hyper::Response::builder()
        .status(status_code)
        .header("X-Request-ID", &request_id);

    // Parse and add response headers
    let mut content_length: Option<usize> = None;
    let mut is_chunked = false;
    
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            
            if name.to_lowercase() == "content-length" {
                content_length = value.parse().ok();
            }
            if name.to_lowercase() == "transfer-encoding" && value.to_lowercase().contains("chunked") {
                is_chunked = true;
            }
            
            if !is_hop_by_hop_header(name) {
                builder = builder.header(name, value);
            }
        }
    }

    debug!(
        request_id = %request_id,
        status = status_code,
        content_length = ?content_length,
        is_chunked = is_chunked,
        "Response headers parsed"
    );

    // Create a channel for streaming response body
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    
    // Send initial body data if any
    if !initial_body.is_empty() {
        let _ = tx.send(Ok(Bytes::from(initial_body.clone()))).await;
    }

    // Spawn task to stream remaining response body
    let request_id_clone = request_id.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        let mut total_read = initial_body.len();
        
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => {
                    debug!(request_id = %request_id_clone, total_bytes = total_read, "Response stream complete");
                    break;
                }
                Ok(n) => {
                    total_read += n;
                    if tx.send(Ok(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        debug!(request_id = %request_id_clone, "Response receiver dropped");
                        break;
                    }
                }
                Err(e) => {
                    error!(request_id = %request_id_clone, "Error reading response body: {}", e);
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    // Build streaming response body
    let body_stream = ReceiverStream::new(rx);
    let body = Body::from_stream(body_stream);

    Ok(builder
        .body(body)
        .unwrap_or_else(|_| bad_gateway("Failed to build response")))
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn bad_gateway(msg: &str) -> Response {
    (StatusCode::BAD_GATEWAY, msg.to_string()).into_response()
}

fn gateway_timeout(msg: &str) -> Response {
    (StatusCode::GATEWAY_TIMEOUT, msg.to_string()).into_response()
}
