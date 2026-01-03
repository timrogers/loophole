mod client;
mod forwarder;
mod reconnect;
mod tunnel;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use std::net::SocketAddr;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use client::TunnelClient;
use reconnect::ReconnectStrategy;

#[derive(Parser)]
#[command(name = "tunnel-client")]
#[command(about = "Connect to a tunnel server and expose a local service")]
struct Args {
    /// Tunnel server address (e.g., localhost:8080)
    #[arg(long)]
    server: String,

    /// Authentication token
    #[arg(long)]
    token: String,

    /// Subdomain to register
    #[arg(long)]
    subdomain: String,

    /// Local port to forward to
    #[arg(long, default_value = "3000")]
    port: u16,

    /// Local host to forward to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Override Host header for local requests
    #[arg(long)]
    local_host: Option<String>,

    /// Maximum number of reconnection attempts (0 = unlimited)
    #[arg(long, default_value = "0")]
    max_retries: u32,

    /// Timeout for forwarding requests to local server (seconds)
    #[arg(long, default_value = "30")]
    forward_timeout: u64,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Suppress request logging output
    #[arg(long)]
    quiet: bool,

    /// Show QR code for tunnel URL
    #[arg(long)]
    qr: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
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

    let local_addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    println!(
        "{} Forwarding to {}",
        "→".cyan(),
        local_addr.to_string().cyan()
    );

    let mut reconnect = ReconnectStrategy::new();
    let max_retries = args.max_retries;
    let forward_timeout = std::time::Duration::from_secs(args.forward_timeout);
    let quiet = args.quiet;
    let show_qr = args.qr;

    loop {
        // Check if we've exceeded max retries
        if max_retries > 0 && reconnect.attempts() >= max_retries {
            eprintln!(
                "{} Maximum reconnection attempts ({}) exceeded",
                "✗".red(),
                max_retries
            );
            return Err(anyhow::anyhow!("Maximum reconnection attempts exceeded"));
        }

        let client = TunnelClient::new(
            args.server.clone(),
            args.token.clone(),
            args.subdomain.clone(),
        );

        match client.connect().await {
            Ok(conn) => {
                reconnect.reset();
                
                // Print success message
                println!("{} Connected to {}", "✓".green(), args.server.green());
                println!(
                    "{} Tunnel URL: {}",
                    "✓".green(),
                    conn.url.bright_green().bold()
                );
                println!();

                // Show QR code if requested
                if show_qr {
                    print_qr_code(&conn.url);
                }

                // Reunite the split stream for yamux
                let ws = conn.write.reunite(conn.read).expect("reunite failed");

                // Run the tunnel
                if let Err(e) = tunnel::run_tunnel(ws, local_addr, args.local_host.clone(), forward_timeout, quiet).await {
                    eprintln!("{} Tunnel error: {}", "✗".red(), e);
                }
            }
            Err(e) => {
                eprintln!("{} Connection failed: {}", "✗".red(), e);

                // Check if it's a fatal error
                let msg = e.to_string();
                if msg.contains("Invalid token")
                    || msg.contains("Invalid subdomain")
                    || msg.contains("Subdomain already taken")
                {
                    return Err(e);
                }
            }
        }

        println!(
            "{} Connection lost, reconnecting...",
            "!".yellow()
        );
        reconnect.wait().await;
    }
}

fn print_qr_code(url: &str) {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    match QrCode::new(url) {
        Ok(code) => {
            let image = code.render::<unicode::Dense1x2>()
                .dark_color(unicode::Dense1x2::Light)
                .light_color(unicode::Dense1x2::Dark)
                .build();
            println!("{}", image);
            println!();
        }
        Err(e) => {
            eprintln!("{} Failed to generate QR code: {}", "!".yellow(), e);
        }
    }
}
