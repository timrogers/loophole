use anyhow::Result;
use colored::Colorize;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as TokioAsyncReadExt, AsyncWriteExt as TokioAsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

pub async fn forward_request(
    request_bytes: &[u8],
    local_addr: SocketAddr,
    local_host: Option<&str>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    debug!("Forwarding {} bytes to {}", request_bytes.len(), local_addr);

    // Connect to local server with timeout
    let stream = tokio::time::timeout(timeout, TcpStream::connect(local_addr)).await;
    let mut stream = match stream {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!("Failed to connect to local server: {}", e));
        }
        Err(_) => {
            return Err(anyhow::anyhow!("Timeout connecting to local server"));
        }
    };

    // Optionally rewrite Host header
    let request_bytes = if let Some(host) = local_host {
        rewrite_host_header(request_bytes, host)
    } else {
        request_bytes.to_vec()
    };

    // Send request with timeout
    let write_result = tokio::time::timeout(
        timeout,
        async {
            TokioAsyncWriteExt::write_all(&mut stream, &request_bytes).await?;
            stream.shutdown().await?;
            Ok::<_, std::io::Error>(())
        }
    ).await;

    match write_result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!("Failed to write request: {}", e));
        }
        Err(_) => {
            return Err(anyhow::anyhow!("Timeout writing request to local server"));
        }
    }

    // Read response with timeout
    let mut response = Vec::new();
    let read_result = tokio::time::timeout(
        timeout,
        TokioAsyncReadExt::read_to_end(&mut stream, &mut response)
    ).await;

    match read_result {
        Ok(Ok(_)) => {
            debug!("Received {} bytes from local server", response.len());
            Ok(response)
        }
        Ok(Err(e)) => {
            Err(anyhow::anyhow!("Failed to read response: {}", e))
        }
        Err(_) => {
            Err(anyhow::anyhow!("Timeout reading response from local server"))
        }
    }
}

fn rewrite_host_header(request: &[u8], new_host: &str) -> Vec<u8> {
    // Find the headers section
    let request_str = match std::str::from_utf8(request) {
        Ok(s) => s,
        Err(_) => return request.to_vec(),
    };

    // Find Host header and replace it
    let mut lines: Vec<&str> = request_str.lines().collect();
    for line in &mut lines {
        if line.to_lowercase().starts_with("host:") {
            // We can't modify &str, so we'll rebuild the request
            break;
        }
    }

    // Rebuild with new host
    let mut result = String::new();
    for line in request_str.lines() {
        if line.to_lowercase().starts_with("host:") {
            result.push_str(&format!("Host: {}\r\n", new_host));
        } else {
            result.push_str(line);
            result.push_str("\r\n");
        }
    }

    result.into_bytes()
}

pub async fn handle_tunnel_stream<S>(mut stream: S, local_addr: SocketAddr, local_host: Option<String>, timeout: Duration, quiet: bool)
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin,
{
    let start_time = Instant::now();
    
    // Read request from tunnel
    let mut request_bytes = Vec::new();
    let mut buf = [0u8; 8192];
    
    // Read until we see end of headers or read fails
    loop {
        match AsyncReadExt::read(&mut stream, &mut buf).await {
            Ok(0) => {
                debug!("Stream read returned 0 bytes");
                break;
            }
            Ok(n) => {
                debug!("Read {} bytes from tunnel stream", n);
                request_bytes.extend_from_slice(&buf[..n]);
                
                // For HTTP/1.1, detect end of headers
                if let Some(pos) = find_header_end(&request_bytes) {
                    // Check if there's a Content-Length header
                    let header_part = &request_bytes[..pos];
                    if let Ok(header_str) = std::str::from_utf8(header_part) {
                        let content_length = parse_content_length(header_str);
                        let body_start = pos + 4;
                        let body_received = request_bytes.len() - body_start;
                        
                        if body_received >= content_length {
                            debug!("Request complete: headers + {} byte body", content_length);
                            break;
                        }
                    } else {
                        // No Content-Length or not text, assume no body
                        break;
                    }
                }
            }
            Err(e) => {
                eprintln!("{} Failed to read from tunnel stream: {}", "✗".red(), e);
                return;
            }
        }
    }

    if request_bytes.is_empty() {
        debug!("Empty request received");
        return;
    }

    // Parse request line for logging
    let request_line = if let Ok(request_str) = std::str::from_utf8(&request_bytes) {
        request_str.lines().next().map(|s| s.to_string())
    } else {
        None
    };

    // Forward to local server
    let response = match forward_request(&request_bytes, local_addr, local_host.as_deref(), timeout).await {
        Ok(r) => r,
        Err(e) => {
            let elapsed = start_time.elapsed();
            if !quiet {
                if let Some(ref req_line) = request_line {
                    let parts: Vec<&str> = req_line.split_whitespace().collect();
                    let method = parts.first().unwrap_or(&"");
                    let path = parts.get(1).unwrap_or(&"");
                    if e.to_string().contains("Timeout") {
                        eprintln!(
                            "{} {} {} {} {}",
                            "←".cyan(),
                            method.yellow(),
                            path,
                            "504 Gateway Timeout".red(),
                            format!("{}ms", elapsed.as_millis()).dimmed()
                        );
                    } else {
                        eprintln!(
                            "{} {} {} {} {}",
                            "←".cyan(),
                            method.yellow(),
                            path,
                            "502 Bad Gateway".red(),
                            format!("{}ms", elapsed.as_millis()).dimmed()
                        );
                    }
                }
            }
            
            if e.to_string().contains("Timeout") {
                b"HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 15\r\n\r\nGateway Timeout".to_vec()
            } else {
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 11\r\n\r\nBad Gateway".to_vec()
            }
        }
    };

    // Parse response for logging
    let elapsed = start_time.elapsed();
    if !quiet {
        if let (Some(req_line), Ok(resp_str)) = (&request_line, std::str::from_utf8(&response)) {
            let parts: Vec<&str> = req_line.split_whitespace().collect();
            let method = parts.first().unwrap_or(&"");
            let path = parts.get(1).unwrap_or(&"");
            
            if let Some(status_line) = resp_str.lines().next() {
                let status_parts: Vec<&str> = status_line.split_whitespace().collect();
                let status_code = status_parts.get(1).unwrap_or(&"");
                let status_text = status_parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                
                let status_display = format!("{} {}", status_code, status_text);
                let status_colored = match status_code.parse::<u16>() {
                    Ok(code) if code < 300 => status_display.green(),
                    Ok(code) if code < 400 => status_display.cyan(),
                    Ok(code) if code < 500 => status_display.yellow(),
                    _ => status_display.red(),
                };
                
                println!(
                    "{} {} {} ({}) {}",
                    "←".cyan(),
                    method.yellow(),
                    path,
                    status_colored,
                    format!("{}ms", elapsed.as_millis()).dimmed()
                );
            }
        }
    }

    debug!("Writing {} bytes response back to tunnel", response.len());

    // Send response back through tunnel
    if let Err(e) = AsyncWriteExt::write_all(&mut stream, &response).await {
        eprintln!("{} Failed to write response to tunnel: {}", "✗".red(), e);
        return;
    }

    // Close the stream to signal we're done
    if let Err(e) = stream.close().await {
        debug!("Stream close error: {}", e);
    }

    debug!("Response sent and stream closed");
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

fn parse_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            if let Some(val) = line.split(':').nth(1) {
                if let Ok(len) = val.trim().parse() {
                    return len;
                }
            }
        }
    }
    0
}
