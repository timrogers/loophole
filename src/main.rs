mod client_config;
mod expose;
mod init;
mod login;
mod proto;
mod server;
mod status;
mod test;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::Level;

#[derive(Parser)]
#[command(name = "loophole")]
#[command(about = "A self-hosted HTTP tunnel")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

fn default_config_path() -> String {
    init::DEFAULT_CONFIG_PATH.to_string()
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new server configuration
    Init {
        /// Domain for tunnels (e.g., tunnel.example.com)
        #[arg(long)]
        domain: Option<String>,

        /// Email for Let's Encrypt certificates
        #[arg(long)]
        email: Option<String>,

        /// Output path for config file
        #[arg(long, short)]
        output: Option<String>,

        /// Install and enable systemd service
        #[arg(long)]
        install: bool,
    },

    /// Run the tunnel server
    Server {
        /// Path to configuration file
        #[arg(short, long, default_value_t = default_config_path())]
        config: String,

        /// Log level
        #[arg(long, default_value = "info")]
        log_level: String,
    },

    /// Login to a tunnel server
    Login {
        /// Server URL (e.g., https://tunnel.example.com)
        #[arg(long)]
        server: Option<String>,

        /// Authentication token
        #[arg(long)]
        token: Option<String>,
    },

    /// Test connection to the tunnel server
    Test {
        /// Server URL (uses saved config if not provided)
        #[arg(long)]
        server: Option<String>,

        /// Authentication token (uses saved config if not provided)
        #[arg(long)]
        token: Option<String>,
    },

    /// Expose a local service through the tunnel
    Expose {
        /// Tunnel server address (uses saved config if not provided)
        #[arg(long)]
        server: Option<String>,

        /// Authentication token (uses saved config if not provided)
        #[arg(long)]
        token: Option<String>,

        /// Subdomain to register (random if not provided)
        #[arg(long)]
        subdomain: Option<String>,

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
    },

    /// Show status of active tunnels on a server
    Status {
        /// Server URL (e.g., https://tunnel.example.com)
        #[arg(long)]
        server: String,

        /// Admin API token
        #[arg(long)]
        admin_token: String,
    },
}

fn parse_log_level(s: &str) -> Level {
    match s.to_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init {
            domain,
            email,
            output,
            install,
        } => init::run(domain, email, output, install),
        Commands::Server { config, log_level } => {
            let level = parse_log_level(&log_level);
            server::run(&config, level).await
        }
        Commands::Login { server, token } => login::run(server, token).await,
        Commands::Test { server, token } => test::run(server, token).await,
        Commands::Expose {
            server,
            token,
            subdomain,
            port,
            host,
            local_host,
            max_retries,
            forward_timeout,
            log_level,
            quiet,
            qr,
        } => {
            let level = parse_log_level(&log_level);
            expose::run(
                server,
                token,
                subdomain,
                host,
                port,
                local_host,
                max_retries,
                forward_timeout,
                level,
                quiet,
                qr,
            )
            .await
        }
        Commands::Status {
            server,
            admin_token,
        } => status::run(server, admin_token).await,
    }
}
