use colored::Colorize;
use futures::io::{AsyncReadExt as FuturesAsyncReadExt, AsyncWriteExt as FuturesAsyncWriteExt};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

/// Handle a tunnel stream by connecting to local server and proxying bidirectionally
pub async fn handle_tunnel_stream<S>(mut tunnel_stream: S, local_addr: SocketAddr, local_host: Option<String>, _timeout: Duration, quiet: bool)
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + Send + 'static,
{
    let start_time = Instant::now();
    
    // Read request headers from tunnel to get method/path for logging
    let mut header_buf = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end;
    
    loop {
        match tunnel_stream.read(&mut buf).await {
            Ok(0) => {
                debug!("Tunnel stream closed before headers");
                return;
            }
            Ok(n) => {
                header_buf.extend_from_slice(&buf[..n]);
                if let Some(pos) = find_header_end(&header_buf) {
                    header_end = pos;
                    break;
                }
                if header_buf.len() > 65536 {
                    eprintln!("{} Request headers too large", "✗".red());
                    return;
                }
            }
            Err(e) => {
                debug!("Failed to read from tunnel: {}", e);
                return;
            }
        }
    }

    // Parse request line for logging
    let request_line = std::str::from_utf8(&header_buf[..header_end])
        .ok()
        .and_then(|s| s.lines().next())
        .map(|s| s.to_string());
    
    // Optionally rewrite Host header
    let request_data = if let Some(ref host) = local_host {
        rewrite_host_header(&header_buf, host)
    } else {
        header_buf
    };

    // Connect to local server
    let local_stream = match TcpStream::connect(local_addr).await {
        Ok(s) => s,
        Err(e) => {
            let elapsed = start_time.elapsed();
            if !quiet {
                if let Some(ref req_line) = request_line {
                    let parts: Vec<&str> = req_line.split_whitespace().collect();
                    let method = parts.first().unwrap_or(&"");
                    let path = parts.get(1).unwrap_or(&"");
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
            // Send error response back through tunnel
            let error_response = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 26\r\n\r\nCannot connect to backend";
            let _ = tunnel_stream.write_all(error_response).await;
            let _ = tunnel_stream.close().await;
            debug!("Failed to connect to local server: {}", e);
            return;
        }
    };

    let (mut local_read, mut local_write) = local_stream.into_split();
    
    // Write buffered request data to local server
    if let Err(e) = local_write.write_all(&request_data).await {
        debug!("Failed to write to local server: {}", e);
        let error_response = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 24\r\n\r\nFailed to send request";
        let _ = tunnel_stream.write_all(error_response).await;
        let _ = tunnel_stream.close().await;
        return;
    }

    // Split the tunnel stream into read and write halves
    let (mut tunnel_read, mut tunnel_write) = tunnel_stream.split();

    // Bidirectional copy between tunnel and local server
    let tunnel_to_local = async move {
        let mut buf = [0u8; 8192];
        loop {
            match tunnel_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if local_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = local_write.shutdown().await;
    };

    let local_to_tunnel = async move {
        let mut buf = [0u8; 8192];
        let mut first_read = true;
        let mut status_code: Option<u16> = None;
        let mut total_bytes = 0usize;
        
        loop {
            match local_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes += n;
                    
                    // Parse status from first chunk
                    if first_read {
                        first_read = false;
                        if let Ok(s) = std::str::from_utf8(&buf[..n.min(100)]) {
                            if let Some(line) = s.lines().next() {
                                let parts: Vec<&str> = line.split_whitespace().collect();
                                status_code = parts.get(1).and_then(|s| s.parse().ok());
                            }
                        }
                    }
                    
                    if tunnel_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // Flush to ensure all data is sent before we finish
        let _ = tunnel_write.flush().await;
        let _ = tunnel_write.close().await;
        
        (status_code, total_bytes)
    };

    let (_, (status_code, _total_bytes)) = tokio::join!(tunnel_to_local, local_to_tunnel);
    
    // Log the completed request
    let elapsed = start_time.elapsed();
    if !quiet {
        if let Some(ref req_line) = request_line {
            let parts: Vec<&str> = req_line.split_whitespace().collect();
            let method = parts.first().unwrap_or(&"");
            let path = parts.get(1).unwrap_or(&"");
            
            let status = status_code.unwrap_or(0);
            let status_display = format!("{}", status);
            let status_colored = match status {
                200..=299 => status_display.green(),
                300..=399 => status_display.cyan(),
                400..=499 => status_display.yellow(),
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

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

fn rewrite_host_header(request: &[u8], new_host: &str) -> Vec<u8> {
    let request_str = match std::str::from_utf8(request) {
        Ok(s) => s,
        Err(_) => return request.to_vec(),
    };

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
