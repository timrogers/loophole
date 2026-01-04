use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, warn};

use crate::tunnel::Tunnel;

pub async fn proxy_request(
    tunnel: Arc<Tunnel>,
    req: hyper::Request<axum::body::Body>,
    client_ip: std::net::IpAddr,
    timeout: Duration,
    is_https: bool,
    max_body_bytes: usize,
) -> Result<hyper::Response<Full<Bytes>>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    tunnel.increment_requests();

    // Convert the request to raw HTTP bytes
    let (parts, body) = req.into_parts();
    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            error!(request_id = %request_id, "Failed to read request body: {}", e);
            return Ok(bad_gateway("Failed to read request body"));
        }
    };

    // Check body size limit
    if body_bytes.len() > max_body_bytes {
        warn!(
            request_id = %request_id,
            body_size = body_bytes.len(),
            max_size = max_body_bytes,
            "Request body too large"
        );
        return Ok(payload_too_large(max_body_bytes));
    }

    // Build raw HTTP request
    let mut request_bytes = Vec::new();
    request_bytes.extend_from_slice(
        format!(
            "{} {} HTTP/1.1\r\n",
            parts.method,
            parts.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
        )
        .as_bytes(),
    );

    // Add headers
    for (name, value) in &parts.headers {
        if !is_hop_by_hop_header(name.as_str()) {
            request_bytes.extend_from_slice(format!("{}: ", name).as_bytes());
            request_bytes.extend_from_slice(value.as_bytes());
            request_bytes.extend_from_slice(b"\r\n");
        }
    }

    // Add forwarded headers
    let proto = if is_https { "https" } else { "http" };
    request_bytes.extend_from_slice(format!("X-Forwarded-For: {}\r\n", client_ip).as_bytes());
    request_bytes.extend_from_slice(format!("X-Forwarded-Proto: {}\r\n", proto).as_bytes());
    request_bytes.extend_from_slice(format!("X-Request-ID: {}\r\n", request_id).as_bytes());

    // End headers
    request_bytes.extend_from_slice(b"\r\n");

    // Add body
    request_bytes.extend_from_slice(&body_bytes);

    debug!(
        request_id = %request_id,
        "Proxying request: {} bytes to subdomain {}",
        request_bytes.len(),
        tunnel.subdomain
    );

    // Send request through tunnel
    let response_result = tokio::time::timeout(
        timeout,
        tunnel.proxy(Bytes::from(request_bytes)),
    )
    .await;

    let response_bytes = match response_result {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            error!(request_id = %request_id, "Proxy error: {}", e);
            return Ok(bad_gateway(&format!("Tunnel error: {}", e)));
        }
        Err(_) => {
            warn!(request_id = %request_id, "Timeout waiting for tunnel response");
            return Ok(gateway_timeout());
        }
    };

    debug!(request_id = %request_id, "Received {} bytes from tunnel", response_bytes.len());

    // Parse HTTP response and add request ID header
    parse_http_response(&response_bytes, &request_id)
}

fn parse_http_response(data: &[u8], request_id: &str) -> Result<hyper::Response<Full<Bytes>>> {
    // Find the end of headers
    let header_end = match find_header_end(data) {
        Some(pos) => pos,
        None => {
            warn!(request_id = %request_id, "Invalid HTTP response: no header end found");
            return Ok(bad_gateway("Invalid response from backend"));
        }
    };

    let header_bytes = &data[..header_end];
    let body_bytes = &data[header_end + 4..]; // Skip \r\n\r\n

    // Parse status line and headers
    let header_str = match std::str::from_utf8(header_bytes) {
        Ok(s) => s,
        Err(_) => {
            warn!(request_id = %request_id, "Invalid UTF-8 in HTTP headers");
            return Ok(bad_gateway("Invalid response from backend"));
        }
    };

    let mut lines = header_str.lines();

    // Parse status line: HTTP/1.1 200 OK
    let status_line = lines.next().unwrap_or("HTTP/1.1 502 Bad Gateway");
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    let status_code = parts
        .get(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(502);

    let mut builder = hyper::Response::builder()
        .status(status_code)
        .header("X-Request-ID", request_id);

    // Parse headers
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if !is_hop_by_hop_header(name) {
                builder = builder.header(name, value);
            }
        }
    }

    Ok(builder
        .body(Full::new(Bytes::copy_from_slice(body_bytes)))
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

fn bad_gateway(msg: &str) -> hyper::Response<Full<Bytes>> {
    hyper::Response::builder()
        .status(502)
        .body(Full::new(Bytes::from(msg.to_string())))
        .unwrap()
}

fn gateway_timeout() -> hyper::Response<Full<Bytes>> {
    hyper::Response::builder()
        .status(504)
        .body(Full::new(Bytes::from("Gateway Timeout")))
        .unwrap()
}

fn payload_too_large(max_bytes: usize) -> hyper::Response<Full<Bytes>> {
    hyper::Response::builder()
        .status(413)
        .body(Full::new(Bytes::from(format!(
            "Request body too large. Maximum allowed: {} bytes",
            max_bytes
        ))))
        .unwrap()
}
