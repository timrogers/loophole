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

fn prompt_yes_no(message: &str, default: bool) -> Result<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    print!("{} {}: ", message, hint);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    if input.is_empty() {
        Ok(default)
    } else {
        Ok(input == "y" || input == "yes")
    }
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

fn validate_config_path(output_path: &PathBuf) -> Result<()> {
    // Create parent directory if needed
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).context(format!(
                "Failed to create directory {}. Try running with sudo.",
                parent.display()
            ))?;
        }
    }

    // Check if we can write to the path by creating/opening the file
    let can_write = if output_path.exists() {
        // Check if we can write to existing file
        fs::OpenOptions::new()
            .write(true)
            .open(output_path)
            .is_ok()
    } else {
        // Try to create the file, then remove it
        match fs::File::create(output_path) {
            Ok(_) => {
                let _ = fs::remove_file(output_path);
                true
            }
            Err(_) => false,
        }
    };

    if !can_write {
        anyhow::bail!(
            "Cannot write to {}. Try running with sudo.",
            output_path.display()
        );
    }

    Ok(())
}

pub fn run(domain: Option<String>, email: Option<String>, output: Option<String>, install: bool) -> Result<()> {
    // Validate config path early, before prompting for input
    let output_path = PathBuf::from(output.unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string()));
    validate_config_path(&output_path)?;

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

    let token = generate_token("tk");

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

[tokens.{token}]
# Token with admin privileges (can access admin API)
admin = true

# Example: non-admin token
# [tokens.tk_example123]
# admin = false

[limits]
# Timeout for proxied requests (seconds)
# request_timeout_secs = 30

# Maximum request body size (bytes)
# max_request_body_bytes = 10485760

# Disconnect tunnels idle for this long (seconds)
# idle_tunnel_timeout_secs = 3600

[https]
# HTTPS configuration with automatic Let's Encrypt certificates
email = "{email}"

# Directory to store certificates
certs_dir = "/var/lib/loophole/certs"

# Use Let's Encrypt staging for testing (avoids rate limits)
# staging = false
"#
    );

    // Check if file already exists
    if output_path.exists() {
        let overwrite = prompt_yes_no(
            &format!("Config file already exists at {}. Overwrite?", output_path.display()),
            false,
        )?;
        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
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
        "{} Generated token: {} {}",
        "✓".green(),
        token.cyan(),
        "(admin)".dimmed()
    );
    println!();

    // Install systemd service - either from --install flag or interactive prompt
    let should_install = if install {
        true
    } else {
        prompt_yes_no("Install systemd service to run in the background?", true)?
    };

    if should_install {
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

    if should_install {
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
    }
    println!();

    // Print client connection example with copyable commands
    println!("{}", "Connect a client:".bold());
    println!();
    println!("  Store your credentials with the {} command:", "login".cyan());
    println!();
    println!(
        "    {}",
        format!(
            "loophole login --server {} --token {}",
            domain, token
        )
        .bright_white()
    );
    println!();
    println!("  Test your connection:");
    println!();
    println!("    {}", "loophole test".bright_white());
    println!();
    println!("  Expose a local service:");
    println!();
    println!(
        "    {}",
        "loophole expose --subdomain myapp --port 3000".bright_white()
    );
    println!();

    // Print admin status command
    println!("{}", "Monitor server status:".bold());
    println!();
    println!(
        "    {}",
        "loophole status".bright_white()
    );

    Ok(())
}
