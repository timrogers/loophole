mod acme;
mod compat;
mod config;
mod handler;
mod proxy;
mod registry;
mod router;
mod tls;
mod tunnel;

pub use config::Config;

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use acme::{AcmeClient, ChallengeStore};
use registry::Registry;
use router::{create_acme_router, create_router, ServerState};
use tls::CertManager;

/// Background task that periodically checks for idle tunnels and removes them
async fn idle_tunnel_cleanup_task(
    registry: Arc<Registry>,
    idle_timeout: Duration,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    let check_interval = Duration::from_secs(60); // Check every minute

    loop {
        tokio::select! {
            _ = tokio::time::sleep(check_interval) => {
                let subdomains = registry.subdomains();
                for subdomain in subdomains {
                    if let Some(tunnel) = registry.get(&subdomain) {
                        if tunnel.is_idle(idle_timeout) {
                            info!(
                                subdomain = %subdomain,
                                idle_seconds = tunnel.last_activity().elapsed().as_secs(),
                                "Removing idle tunnel"
                            );
                            registry.deregister(&subdomain);
                        }
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Idle cleanup task shutting down");
                break;
            }
        }
    }
}

pub async fn run(config_path: &str, log_level: Level) -> Result<()> {
    // Crypto provider is already installed in main.rs

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Load config
    let config = Config::load(config_path)?;
    info!("Loaded configuration from {}", config_path);
    info!("Domain: {}", config.server.domain);
    info!("HTTP port: {}", config.server.http_port);

    // Create shutdown signal channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Create challenge store for ACME HTTP-01
    let challenge_store = Arc::new(ChallengeStore::new());

    // Create ACME client and cert manager if configured
    let (_acme_client, cert_manager) = if let Some(ref https_config) = config.https {
        info!("HTTPS enabled with email: {}", https_config.email);
        info!("HTTPS port: {}", config.server.https_port);

        let certs_dir = PathBuf::from(&https_config.certs_dir);
        let directory_url = if https_config.staging {
            "https://acme-staging-v02.api.letsencrypt.org/directory"
        } else {
            &https_config.directory
        };

        // Load custom CA file if specified (for testing with Pebble)
        let additional_roots = if let Some(ref ca_file) = https_config.ca_file {
            info!("Loading additional CA from: {}", ca_file);
            Some(std::fs::read(ca_file).context("Failed to read CA file")?)
        } else {
            None
        };

        let acme_client = Arc::new(
            AcmeClient::new_with_roots(
                &https_config.email,
                directory_url,
                certs_dir.clone(),
                challenge_store.clone(),
                additional_roots.as_deref(),
            )
            .await?,
        );

        let cert_manager = Arc::new(
            CertManager::new(
                certs_dir,
                Some(acme_client.clone()),
                challenge_store.clone(),
                config.server.domain.clone(),
            )
            .await?,
        );

        // Note: Base domain certificate will be requested after HTTP server starts
        // so that ACME HTTP-01 challenges can be served

        (Some(acme_client), Some(cert_manager))
    } else {
        info!("HTTPS not configured, running HTTP only");
        (None, None)
    };

    // Create shared state
    let registry = Arc::new(Registry::new());
    let state = Arc::new(ServerState {
        config: Arc::new(config.clone()),
        registry: registry.clone(),
        cert_manager: cert_manager.clone(),
    });

    // Start idle tunnel cleanup task
    let idle_timeout = Duration::from_secs(config.limits.idle_tunnel_timeout_secs);
    let cleanup_registry = registry.clone();
    let cleanup_shutdown_rx = shutdown_tx.subscribe();
    tokio::spawn(async move {
        idle_tunnel_cleanup_task(cleanup_registry, idle_timeout, cleanup_shutdown_rx).await;
    });

    // Create graceful shutdown signal
    let shutdown_signal = async {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }

        info!("Shutdown signal received, draining connections...");
        let _ = shutdown_tx.send(());
    };

    // Start HTTP server (always runs for ACME challenges and plain HTTP)
    let http_addr = SocketAddr::from(([0, 0, 0, 0], config.server.http_port));
    let http_state = state.clone();
    let http_challenge_store = challenge_store.clone();
    let has_https = cert_manager.is_some();

    let http_handle = tokio::spawn(async move {
        let app = create_acme_router(http_state, http_challenge_store, has_https);
        info!("Starting HTTP server on {}", http_addr);
        let listener = tokio::net::TcpListener::bind(http_addr).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server error: {}", e))
    });

    // Start HTTPS server if configured
    if let Some(cert_manager) = cert_manager {
        let https_addr = SocketAddr::from(([0, 0, 0, 0], config.server.https_port));
        let https_state = state.clone();

        // Request base domain certificate in background (after HTTP server has started)
        let base_domain = config.server.domain.clone();
        let cert_manager_clone = cert_manager.clone();
        tokio::spawn(async move {
            // Give HTTP server a moment to start
            tokio::time::sleep(Duration::from_millis(500)).await;
            
            if !cert_manager_clone.has_cert(&base_domain) {
                info!("Requesting certificate for base domain: {}", base_domain);
                if let Err(e) = cert_manager_clone.request_cert(&base_domain).await {
                    warn!("Failed to get base domain certificate: {}. Clients should connect via http:// until certificate is obtained.", e);
                } else {
                    info!("Base domain certificate ready - clients can now connect via https://");
                }
            }
        });

        let https_handle = tokio::spawn(async move {
            let app = create_router(https_state);
            let tls_config = tls::create_tls_config(cert_manager)?;

            info!("Starting HTTPS server on {}", https_addr);

            let config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

            axum_server::bind_rustls(https_addr, config)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .map_err(|e| anyhow::anyhow!("HTTPS server error: {}", e))
        });

        // Wait for shutdown signal or server error
        tokio::select! {
            res = http_handle => {
                match res {
                    Ok(Ok(())) => info!("HTTP server exited"),
                    Ok(Err(e)) => error!("HTTP server error: {}", e),
                    Err(e) => error!("HTTP server task panicked: {}", e),
                }
            },
            res = https_handle => {
                match res {
                    Ok(Ok(())) => info!("HTTPS server exited"),
                    Ok(Err(e)) => error!("HTTPS server error: {}", e),
                    Err(e) => error!("HTTPS server task panicked: {}", e),
                }
            },
            _ = shutdown_signal => {
                info!("Shutting down gracefully...");
            }
        }
    } else {
        // Just HTTP
        tokio::select! {
            res = http_handle => {
                match res {
                    Ok(Ok(())) => info!("HTTP server exited"),
                    Ok(Err(e)) => error!("HTTP server error: {}", e),
                    Err(e) => error!("HTTP server task panicked: {}", e),
                }
            },
            _ = shutdown_signal => {
                info!("Shutting down gracefully...");
            }
        }
    }

    info!("Server shutdown complete");
    Ok(())
}
