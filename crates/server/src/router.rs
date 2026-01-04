use axum::{
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{header, Request, StatusCode},
    response::{IntoResponse, Json, Redirect, Response},
    routing::{any, delete, get},
    Extension, Router,
};
use axum::extract::ws::WebSocketUpgrade;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::acme::ChallengeStore;
use crate::config::Config;
use crate::proxy::proxy_request;
use crate::registry::Registry;
use crate::tls::CertManager;

pub struct ServerState {
    pub config: Arc<Config>,
    pub registry: Arc<Registry>,
    pub cert_manager: Option<Arc<CertManager>>,
}

/// Create the main router for HTTPS (tunnel connections and proxying)
pub fn create_router(state: Arc<ServerState>) -> Router {
    let mut router = Router::new()
        .route("/*path", any(handle_request))
        .route("/", any(handle_request));
    
    // Add admin routes if enabled
    if let Some(ref admin) = state.config.admin {
        if admin.enabled {
            router = router
                .route("/_admin/tunnels", get(list_tunnels))
                .route("/_admin/tunnels/{subdomain}", delete(delete_tunnel));
        }
    }
    
    router.with_state(state)
}

/// Create the HTTP router that handles ACME challenges and redirects to HTTPS
pub fn create_acme_router(
    state: Arc<ServerState>,
    challenge_store: Arc<ChallengeStore>,
    has_https: bool,
) -> Router {
    // Create ACME challenge handler as a nested router with its own layer
    let acme_router = Router::new()
        .route(
            "/.well-known/acme-challenge/{token}",
            get(handle_acme_challenge),
        )
        .layer(Extension(challenge_store.clone()));

    if has_https {
        // HTTPS mode: serve ACME challenges, allow control path, redirect everything else
        let control_path = state.config.server.control_path.clone();
        let mut router = Router::new()
            // Merge the ACME router first
            .merge(acme_router)
            // Allow WebSocket connections on the control path (for tunnel registration)
            .route(&control_path, any(handle_request));
        
        // Add admin routes if enabled
        if let Some(ref admin) = state.config.admin {
            if admin.enabled {
                router = router
                    .route("/_admin/tunnels", get(list_tunnels))
                    .route("/_admin/tunnels/{subdomain}", delete(delete_tunnel));
            }
        }
        
        // Use fallback for everything else - redirect to HTTPS
        router
            .fallback(redirect_to_https)
            .layer(Extension(challenge_store))
            .with_state(state)
    } else {
        // No HTTPS, serve tunnel traffic on HTTP with ACME challenge support
        let mut router = Router::new()
            .merge(acme_router);
        
        // Add admin routes if enabled
        if let Some(ref admin) = state.config.admin {
            if admin.enabled {
                router = router
                    .route("/_admin/tunnels", get(list_tunnels))
                    .route("/_admin/tunnels/{subdomain}", delete(delete_tunnel));
            }
        }
        
        router
            .route("/*path", any(handle_request))
            .route("/", any(handle_request))
            .layer(Extension(challenge_store))
            .with_state(state)
    }
}

/// Handle ACME HTTP-01 challenge requests
async fn handle_acme_challenge(
    Path(token): Path<String>,
    Extension(challenge_store): Extension<Arc<ChallengeStore>>,
) -> Response {
    info!("ACME challenge request received for token: {}", token);

    match challenge_store.get(&token) {
        Some(key_auth) => {
            info!("Responding to ACME challenge for token: {} with key_auth length: {}", token, key_auth.len());
            (StatusCode::OK, key_auth).into_response()
        }
        None => {
            error!("ACME challenge token NOT FOUND in store: {}", token);
            (StatusCode::NOT_FOUND, "Challenge not found").into_response()
        }
    }
}

/// Redirect HTTP to HTTPS
async fn redirect_to_https(
    State(state): State<Arc<ServerState>>,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path();
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    // Log if this is an ACME challenge that shouldn't have reached here
    if path.starts_with("/.well-known/acme-challenge/") {
        error!("ACME challenge request incorrectly routed to redirect handler! Path: {}, Host: {}", path, host);
    }

    // Remove port from host if present
    let host_without_port = host.split(':').next().unwrap_or(host);

    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let https_port = state.config.server.https_port;
    
    // Build HTTPS URL
    let https_url = if https_port == 443 {
        format!("https://{}{}", host_without_port, path_and_query)
    } else {
        format!("https://{}:{}{}", host_without_port, https_port, path_and_query)
    };

    debug!("Redirecting to HTTPS: {}", https_url);
    Redirect::permanent(&https_url).into_response()
}

async fn handle_request(
    State(state): State<Arc<ServerState>>,
    ws: Option<WebSocketUpgrade>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let path = req.uri().path();
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    debug!(request_id = %request_id, "Request: {} {} from {}", req.method(), path, host);

    // Check if this is a WebSocket upgrade request to the control path
    if path == state.config.server.control_path {
        if let Some(ws) = ws {
            return handle_tunnel_connect(ws, state, addr).await;
        } else {
            return (StatusCode::BAD_REQUEST, "WebSocket upgrade required").into_response();
        }
    }

    // Extract subdomain from Host header
    let subdomain = match extract_subdomain(host, &state.config.server.domain) {
        Some(s) => s,
        None => {
            debug!(request_id = %request_id, "No subdomain found in host: {}", host);
            return (StatusCode::NOT_FOUND, "Unknown subdomain").into_response();
        }
    };

    // Look up tunnel in registry
    let tunnel = match state.registry.get(&subdomain) {
        Some(t) => t,
        None => {
            debug!(request_id = %request_id, "Tunnel not found for subdomain: {}", subdomain);
            return (StatusCode::NOT_FOUND, "Tunnel not found").into_response();
        }
    };

    // Determine if this is HTTPS based on whether ACME is configured
    let is_https = state.config.acme.is_some();

    // Proxy the request
    let timeout = Duration::from_secs(state.config.limits.request_timeout_secs);
    let max_body_bytes = state.config.limits.max_request_body_bytes;
    
    match proxy_request(tunnel, req, addr.ip(), timeout, is_https, max_body_bytes, &request_id).await {
        Ok(response) => response.into_response(),
        Err(e) => {
            error!(request_id = %request_id, "Proxy error: {}", e);
            (StatusCode::BAD_GATEWAY, "Proxy error").into_response()
        }
    }
}

fn extract_subdomain<'a>(host: &'a str, domain: &str) -> Option<String> {
    // Remove port from host if present
    let host = host.split(':').next().unwrap_or(host);
    
    // Check if host ends with the domain
    if host == domain {
        return None;
    }
    
    // For localhost testing: myapp.localhost -> myapp
    if domain == "localhost" && host.ends_with(".localhost") {
        let subdomain = host.strip_suffix(".localhost")?;
        return Some(subdomain.to_string());
    }

    // Standard case: myapp.tunnel.example.com -> myapp
    let suffix = format!(".{}", domain);
    if host.ends_with(&suffix) {
        let subdomain = host.strip_suffix(&suffix)?;
        // Only take the first part (no nested subdomains)
        if !subdomain.contains('.') {
            return Some(subdomain.to_string());
        }
    }

    None
}

async fn handle_tunnel_connect(
    ws: WebSocketUpgrade,
    state: Arc<ServerState>,
    addr: SocketAddr,
) -> Response {
    info!("New tunnel connection from {}", addr);

    ws.on_upgrade(move |socket| async move {
        if let Err(e) = crate::handler::handle_websocket(socket, state, addr).await {
            error!("WebSocket handler error: {}", e);
        }
    })
}

// Admin endpoint types
#[derive(Serialize)]
struct TunnelInfo {
    subdomain: String,
    created_at_secs: u64,
    request_count: u64,
    idle_secs: u64,
}

#[derive(Serialize)]
struct TunnelListResponse {
    tunnels: Vec<TunnelInfo>,
    count: usize,
}

#[derive(Serialize)]
struct AdminError {
    error: String,
}

/// Validate admin authorization header
fn validate_admin_auth(req: &Request<Body>, config: &Config) -> Result<(), Response> {
    let admin = config.admin.as_ref().ok_or_else(|| {
        (StatusCode::NOT_FOUND, "Admin endpoint not enabled").into_response()
    })?;
    
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(AdminError { error: "Authorization header required".to_string() })).into_response()
        })?;
    
    // Expect "Bearer <token>" format
    let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        (StatusCode::UNAUTHORIZED, Json(AdminError { error: "Invalid authorization format".to_string() })).into_response()
    })?;
    
    if token != admin.token {
        return Err((StatusCode::UNAUTHORIZED, Json(AdminError { error: "Invalid admin token".to_string() })).into_response());
    }
    
    Ok(())
}

/// List all active tunnels
async fn list_tunnels(
    State(state): State<Arc<ServerState>>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = validate_admin_auth(&req, &state.config) {
        return resp;
    }
    
    let subdomains = state.registry.subdomains();
    let mut tunnels = Vec::with_capacity(subdomains.len());
    
    for subdomain in subdomains {
        if let Some(tunnel) = state.registry.get(&subdomain) {
            tunnels.push(TunnelInfo {
                subdomain: tunnel.subdomain.clone(),
                created_at_secs: tunnel.created_at.elapsed().as_secs(),
                request_count: tunnel.request_count.load(std::sync::atomic::Ordering::Relaxed),
                idle_secs: tunnel.last_activity().elapsed().as_secs(),
            });
        }
    }
    
    let count = tunnels.len();
    info!("Admin: listed {} tunnels", count);
    
    Json(TunnelListResponse { tunnels, count }).into_response()
}

/// Force disconnect a tunnel
async fn delete_tunnel(
    State(state): State<Arc<ServerState>>,
    Path(subdomain): Path<String>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = validate_admin_auth(&req, &state.config) {
        return resp;
    }
    
    if state.registry.get(&subdomain).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(AdminError { error: format!("Tunnel '{}' not found", subdomain) }),
        ).into_response();
    }
    
    state.registry.deregister(&subdomain);
    info!("Admin: force disconnected tunnel '{}'", subdomain);
    
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_subdomain() {
        assert_eq!(
            extract_subdomain("myapp.localhost", "localhost"),
            Some("myapp".to_string())
        );
        assert_eq!(
            extract_subdomain("myapp.localhost:8080", "localhost"),
            Some("myapp".to_string())
        );
        assert_eq!(
            extract_subdomain("myapp.tunnel.example.com", "tunnel.example.com"),
            Some("myapp".to_string())
        );
        assert_eq!(extract_subdomain("localhost", "localhost"), None);
        assert_eq!(
            extract_subdomain("tunnel.example.com", "tunnel.example.com"),
            None
        );
    }
}
