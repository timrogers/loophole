mod acme;
mod compat;
mod config;
mod handler;
mod proxy;
mod registry;
mod router;
mod tls;
mod tunnel;

use anyhow::{Context, Result};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use acme::{AcmeClient, ChallengeStore};
use config::Config;
use registry::Registry;
use router::{create_router, create_acme_router, ServerState};
use tls::CertManager;

#[derive(Parser)]
#[command(name = "tunnel-server")]
#[command(about = "A self-hosted HTTP tunnel server")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config/server.toml")]
    config: String,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,
}

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

#[tokio::main]
async fn main() -> Result<()> {
    // Install the default crypto provider for rustls
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let args = Args::parse();

    // Initialize logging
    let level = match args.log_level.to_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    let subscriber = FmtSubscriber::builder().with_max_level(level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Load config
    let config = Config::load(&args.config)?;
    info!("Loaded configuration from {}", args.config);
    info!("Domain: {}", config.server.domain);
    info!("HTTP port: {}", config.server.http_port);

    // Create shutdown signal channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Create challenge store for ACME HTTP-01
    let challenge_store = Arc::new(ChallengeStore::new());

    // Create ACME client and cert manager if configured
    let (_acme_client, cert_manager) = if let Some(ref acme_config) = config.acme {
        info!("ACME enabled with email: {}", acme_config.email);
        info!("HTTPS port: {}", config.server.https_port);

        let certs_dir = PathBuf::from(&acme_config.certs_dir);
        let directory_url = if acme_config.staging {
            "https://acme-staging-v02.api.letsencrypt.org/directory"
        } else {
            &acme_config.directory
        };

        // Load custom CA file if specified (for testing with Pebble)
        let additional_roots = if let Some(ref ca_file) = acme_config.ca_file {
            info!("Loading additional CA from: {}", ca_file);
            Some(std::fs::read(ca_file).context("Failed to read CA file")?)
        } else {
            None
        };

        let acme_client = Arc::new(
            AcmeClient::new_with_roots(
                &acme_config.email,
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

        (Some(acme_client), Some(cert_manager))
    } else {
        info!("ACME not configured, running HTTP only");
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
                if let Err(e) = res {
                    warn!("HTTP server error: {}", e);
                }
            },
            res = https_handle => {
                if let Err(e) = res {
                    warn!("HTTPS server error: {}", e);
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
                if let Err(e) = res {
                    warn!("HTTP server error: {}", e);
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
