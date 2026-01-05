use anyhow::{Context, Result};
use colored::Colorize;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct TunnelInfo {
    subdomain: String,
    created_at_secs: u64,
    request_count: u64,
    idle_secs: u64,
}

#[derive(Debug, Deserialize)]
struct TunnelListResponse {
    tunnels: Vec<TunnelInfo>,
    count: usize,
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let mins = secs / 60;
        let secs = secs % 60;
        if secs == 0 {
            format!("{}m", mins)
        } else {
            format!("{}m {}s", mins, secs)
        }
    } else {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins == 0 {
            format!("{}h", hours)
        } else {
            format!("{}h {}m", hours, mins)
        }
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub async fn run(server: String, admin_token: String) -> Result<()> {
    let url = format!("{}/_admin/tunnels", server);

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", admin_token))
        .send()
        .await
        .context("Failed to connect to server")?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("Invalid admin token");
    }

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("Admin API not enabled on server");
    }

    if !response.status().is_success() {
        anyhow::bail!("Server returned error: {}", response.status());
    }

    let data: TunnelListResponse = response
        .json()
        .await
        .context("Failed to parse server response")?;

    // Print header
    println!(
        "{} {}",
        "Active Tunnels:".bold(),
        data.count.to_string().cyan()
    );
    println!();

    if data.tunnels.is_empty() {
        println!("{}", "No active tunnels".dimmed());
        return Ok(());
    }

    // Print table header
    println!(
        "{:<20} {:<12} {:<12} {:<12}",
        "SUBDOMAIN".dimmed(),
        "AGE".dimmed(),
        "REQUESTS".dimmed(),
        "IDLE".dimmed()
    );

    // Print tunnels
    for tunnel in data.tunnels {
        println!(
            "{:<20} {:<12} {:<12} {:<12}",
            tunnel.subdomain.green(),
            format_duration(tunnel.created_at_secs),
            format_count(tunnel.request_count),
            format_duration(tunnel.idle_secs),
        );
    }

    Ok(())
}
