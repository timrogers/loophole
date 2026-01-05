use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{self, Write};

use crate::client_config::ClientConfig;

fn prompt(message: &str) -> Result<String> {
    print!("{}: ", message);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn prompt_secret(message: &str) -> Result<String> {
    print!("{}: ", message);
    io::stdout().flush()?;
    let input = rpassword::read_password().context("Failed to read password")?;
    Ok(input.trim().to_string())
}

pub async fn run(server: Option<String>, token: Option<String>) -> Result<()> {
    let server = match server {
        Some(s) => s,
        None => {
            let input = prompt("Server URL (e.g., https://tunnel.example.com)")?;
            if input.is_empty() {
                anyhow::bail!("Server URL is required");
            }
            input
        }
    };

    // Validate and normalize the server URL
    let server = if !server.starts_with("http://") && !server.starts_with("https://") {
        // Default to https:// if no scheme provided
        format!("https://{}", server)
    } else {
        server
    };

    // Validate URL format
    if url::Url::parse(&server).is_err() {
        anyhow::bail!("Invalid server URL: {}", server);
    }

    let token = match token {
        Some(t) => t,
        None => {
            let input = prompt_secret("Token")?;
            if input.is_empty() {
                anyhow::bail!("Token is required");
            }
            input
        }
    };

    // Validate by attempting a test connection
    println!("{} Validating credentials...", "→".cyan());

    let result = crate::test::check_connection(&server, &token).await;

    match result {
        Ok(()) => {
            // Save config
            let config = ClientConfig::new(server.clone(), token);
            let path = config.save()?;

            println!("{} Logged in to {}", "✓".green(), server.green());
            println!("{} Credentials saved to {}", "✓".green(), path.display());
            println!();
            println!("You can now expose local services:");
            println!(
                "  {}",
                "loophole expose --subdomain myapp --port 3000".bright_white()
            );
        }
        Err(e) => {
            anyhow::bail!("Login failed: {}", e);
        }
    }

    Ok(())
}
