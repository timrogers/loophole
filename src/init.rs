use anyhow::{Context, Result};
use colored::Colorize;
use rand::Rng;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/loophole/server.toml";
const SYSTEMD_SERVICE_PATH: &str = "/etc/systemd/system/loophole.service";

fn generate_token(prefix: &str) -> String {
    let mut rng = rand::rng();
    let random: [u8; 16] = rng.random();
    let hex: String = random.iter().map(|b| format!("{:02x}", b)).collect();
    format!("{}_{}", prefix, hex)
}

fn prompt(message: &str) -> Result<String> {
    print!("{}: ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn install_systemd_service(config_path: &PathBuf) -> Result<()> {
    // Find the loophole binary
    let binary_path = std::env::current_exe()
        .context("Failed to determine loophole binary path")?;
    
    let exec_start = if config_path.to_string_lossy() == DEFAULT_CONFIG_PATH {
        format!("{} server", binary_path.display())
    } else {
        format!("{} server --config {}", binary_path.display(), config_path.display())
    };

    let service = format!(
        r#"[Unit]
Description=Loophole Tunnel Server
After=network.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"#
    );

    // Write the service file
    fs::write(SYSTEMD_SERVICE_PATH, &service)
        .context(format!("Failed to write systemd service to {}. Try running with sudo.", SYSTEMD_SERVICE_PATH))?;
    
    println!("{} Created {}", "✓".green(), SYSTEMD_SERVICE_PATH);

    // Reload systemd
    let status = Command::new("systemctl")
        .args(["daemon-reload"])
        .status()
        .context("Failed to run systemctl daemon-reload")?;
    
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed");
    }
    println!("{} Reloaded systemd", "✓".green());

    // Enable and start the service
    let status = Command::new("systemctl")
        .args(["enable", "--now", "loophole"])
        .status()
        .context("Failed to run systemctl enable --now loophole")?;
    
    if !status.success() {
        anyhow::bail!("systemctl enable --now loophole failed");
    }
    println!("{} Enabled and started loophole service", "✓".green());

    // Give it a moment to start, then check if it's running
    std::thread::sleep(std::time::Duration::from_secs(2));
    
    let output = Command::new("systemctl")
        .args(["is-active", "loophole"])
        .output()
        .context("Failed to check service status")?;
    
    let is_active = String::from_utf8_lossy(&output.stdout).trim() == "active";
    
    if is_active {
        println!("{} Service is running", "✓".green());
    } else {
        println!("{} Service may not have started correctly", "!".yellow());
        println!("  Check logs with: {}", "sudo journalctl -u loophole -f".bright_white());
    }

    Ok(())
}

pub fn run(domain: Option<String>, email: Option<String>, output: Option<String>, install: bool) -> Result<()> {
    let domain = match domain {
        Some(d) => d,
        None => {
            let input = prompt("Domain for tunnels (e.g., tunnel.example.com)")?;
            if input.is_empty() {
                anyhow::bail!("Domain is required");
            }
            input
        }
    };

    let email = match email {
        Some(e) => e,
        None => {
            let input = prompt("Email for Let's Encrypt")?;
            if input.is_empty() {
                anyhow::bail!("Email is required");
            }
            input
        }
    };

    let tunnel_token = generate_token("tk");
    let admin_token = generate_token("admin");

    let config = format!(
        r#"# Loophole Server Configuration
version = 1

[server]
# Base domain for tunnels (e.g., tunnel.example.com)
# Clients will get subdomains like myapp.tunnel.example.com
domain = "{domain}"

# HTTP port - used for ACME challenges and HTTP->HTTPS redirect
# Default: 80 (required for Let's Encrypt)
# http_port = 80

# HTTPS port - used for tunnel traffic
# Default: 443
# https_port = 443

[tokens]
# Authentication tokens for clients
# Format: "token" = max_tunnels (0 = unlimited)
"{tunnel_token}" = 0

[limits]
# Timeout for proxied requests (seconds)
# request_timeout_secs = 30

# Maximum request body size (bytes)
# max_request_body_bytes = 10485760

# Disconnect tunnels idle for this long (seconds)
# idle_tunnel_timeout_secs = 3600

[acme]
# Let's Encrypt configuration for automatic HTTPS
email = "{email}"

# Directory to store certificates
certs_dir = "/var/lib/loophole/certs"

# Use Let's Encrypt staging for testing (avoids rate limits)
# staging = false

[admin]
# Admin API for monitoring and management
enabled = true
token = "{admin_token}"
"#
    );

    let output_path = PathBuf::from(output.unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string()));

    // Create parent directory if needed
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).context(format!(
            "Failed to create directory {}",
            parent.display()
        ))?;
    }

    // Check if file already exists
    if output_path.exists() {
        anyhow::bail!(
            "Config file already exists at {}. Remove it first or use --output to specify a different path.",
            output_path.display()
        );
    }

    // Write config
    fs::write(&output_path, &config).context(format!(
        "Failed to write config to {}",
        output_path.display()
    ))?;

    // Create certs directory
    let certs_dir = PathBuf::from("/var/lib/loophole/certs");
    if let Err(e) = fs::create_dir_all(&certs_dir) {
        eprintln!(
            "{} Could not create certs directory {}: {}",
            "!".yellow(),
            certs_dir.display(),
            e
        );
        eprintln!("  You may need to create it manually with appropriate permissions.");
    }

    // Print success
    println!("{} Created {}", "✓".green(), output_path.display());
    println!(
        "{} Generated tunnel token: {}",
        "✓".green(),
        tunnel_token.cyan()
    );
    println!(
        "{} Generated admin token: {}",
        "✓".green(),
        admin_token.cyan()
    );
    println!();

    // Install systemd service if requested
    if install {
        install_systemd_service(&output_path)?;
        println!();
    }

    // Print next steps
    println!("{}", "Next steps:".bold());
    println!();
    println!("  {}. {} Configure DNS", "1".cyan(), "→".dimmed());
    println!(
        "     Add a wildcard A record pointing to your server:"
    );
    println!(
        "       {}  A  <your-server-ip>",
        format!("*.{}", domain).bright_white()
    );
    println!();
    println!("  {}. {} Open firewall ports", "2".cyan(), "→".dimmed());
    println!("     The server needs ports 80 (HTTP) and 443 (HTTPS) open:");
    println!(
        "       {}",
        "sudo ufw allow 80/tcp && sudo ufw allow 443/tcp".bright_white()
    );
    println!("     or:");
    println!(
        "       {}",
        "sudo firewall-cmd --add-port=80/tcp --add-port=443/tcp --permanent".bright_white()
    );
    println!();

    if install {
        // Service is already running
        println!("  {}. {} Check service status", "3".cyan(), "→".dimmed());
        println!(
            "       {}",
            "sudo systemctl status loophole".bright_white()
        );
    } else {
        // Manual start instructions
        println!("  {}. {} Start the server", "3".cyan(), "→".dimmed());
        if output_path.to_string_lossy() == DEFAULT_CONFIG_PATH {
            println!("       {}", "loophole server".bright_white());
        } else {
            println!(
                "       {}",
                format!("loophole server --config {}", output_path.display()).bright_white()
            );
        }
        println!();
        println!(
            "  {}. {} (Optional) Set up systemd service",
            "4".cyan(),
            "→".dimmed()
        );
        println!("     Create {} with:",
            "/etc/systemd/system/loophole.service".bright_white()
        );
        println!();
        println!("       [Unit]");
        println!("       Description=Loophole Tunnel Server");
        println!("       After=network.target");
        println!();
        println!("       [Service]");
        println!("       Type=simple");
        if output_path.to_string_lossy() == DEFAULT_CONFIG_PATH {
            println!("       ExecStart=/usr/local/bin/loophole server");
        } else {
            println!(
                "       ExecStart=/usr/local/bin/loophole server --config {}",
                output_path.display()
            );
        }
        println!("       Restart=always");
        println!("       RestartSec=5");
        println!();
        println!("       [Install]");
        println!("       WantedBy=multi-user.target");
        println!();
        println!("     Then enable and start:");
        println!(
            "       {}",
            "sudo systemctl enable --now loophole".bright_white()
        );
        println!();
        println!("     Or use {} during init to set this up automatically.", "--install".cyan());
    }
    println!();

    // Print client connection example
    println!("{}", "Connect a client:".bold());
    println!();
    println!("  On the client machine, login and test the connection:");
    println!(
        "    {}",
        format!(
            "loophole login --server {} --token {}",
            domain, tunnel_token
        )
        .bright_white()
    );
    println!("    {}", "loophole test".bright_white());
    println!();
    println!("  Then expose a local service:");
    println!(
        "    {}",
        "loophole expose --subdomain myapp --port 3000".bright_white()
    );

    Ok(())
}
