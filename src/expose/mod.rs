mod client;
mod forwarder;
mod reconnect;
mod tunnel;

use anyhow::Result;
use colored::Colorize;
use std::net::SocketAddr;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use client::TunnelClient;
use reconnect::ReconnectStrategy;

use crate::client_config::ClientConfig;

fn generate_subdomain() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let adjectives = ["quick", "bright", "calm", "eager", "fancy", "gentle", "happy", "jolly", "kind", "lively"];
    let nouns = ["fox", "owl", "bear", "wolf", "deer", "hawk", "lynx", "seal", "duck", "frog"];
    let adj = adjectives[rng.random_range(0..adjectives.len())];
    let noun = nouns[rng.random_range(0..nouns.len())];
    let num: u16 = rng.random_range(100..1000);
    format!("{}-{}-{}", adj, noun, num)
}

pub async fn run(
    server: Option<String>,
    token: Option<String>,
    subdomain: Option<String>,
    host: String,
    port: u16,
    local_host: Option<String>,
    max_retries: u32,
    forward_timeout_secs: u64,
    log_level: Level,
    quiet: bool,
    show_qr: bool,
) -> Result<()> {
    // Load from config if not provided
    let (server, token) = match (server, token) {
        (Some(s), Some(t)) => (s, t),
        (s, t) => {
            let config = ClientConfig::load()?
                .ok_or_else(|| anyhow::anyhow!("Not logged in. Run 'loophole login' first, or provide --server and --token."))?;
            (s.unwrap_or(config.server), t.unwrap_or(config.token))
        }
    };

    // Generate subdomain if not provided
    let subdomain = subdomain.unwrap_or_else(generate_subdomain);

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let local_addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    println!(
        "{} Forwarding to {}",
        "→".cyan(),
        local_addr.to_string().cyan()
    );

    let mut reconnect = ReconnectStrategy::new();
    let forward_timeout = std::time::Duration::from_secs(forward_timeout_secs);

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

        let client = TunnelClient::new(server.clone(), token.clone(), subdomain.clone());

        match client.connect().await {
            Ok(conn) => {
                reconnect.reset();

                // Print success message
                println!("{} Connected to {}", "✓".green(), server.green());
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
                if let Err(e) =
                    tunnel::run_tunnel(ws, local_addr, local_host.clone(), forward_timeout, quiet)
                        .await
                {
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

        println!("{} Connection lost, reconnecting...", "!".yellow());
        reconnect.wait().await;
    }
}

fn print_qr_code(url: &str) {
    use qrcode::render::unicode;
    use qrcode::QrCode;

    match QrCode::new(url) {
        Ok(code) => {
            let image = code
                .render::<unicode::Dense1x2>()
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
