use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

const CONFIG_VERSION: u32 = 1;

/// Environment variable names for Docker/container configuration
pub mod env {
    pub const DOMAIN: &str = "LOOPHOLE_DOMAIN";
    pub const HTTP_PORT: &str = "LOOPHOLE_HTTP_PORT";
    pub const HTTPS_PORT: &str = "LOOPHOLE_HTTPS_PORT";
    pub const TOKENS: &str = "LOOPHOLE_TOKENS";
    pub const ADMIN_TOKENS: &str = "LOOPHOLE_ADMIN_TOKENS";
    pub const ACME_EMAIL: &str = "LOOPHOLE_ACME_EMAIL";
    pub const ACME_STAGING: &str = "LOOPHOLE_ACME_STAGING";
    pub const ACME_DIRECTORY: &str = "LOOPHOLE_ACME_DIRECTORY";
    pub const CERTS_DIR: &str = "LOOPHOLE_CERTS_DIR";
    pub const REQUEST_TIMEOUT: &str = "LOOPHOLE_REQUEST_TIMEOUT_SECS";
    pub const MAX_BODY: &str = "LOOPHOLE_MAX_REQUEST_BODY_BYTES";
    pub const IDLE_TIMEOUT: &str = "LOOPHOLE_IDLE_TUNNEL_TIMEOUT_SECS";
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    pub server: ServerConfig,
    pub tokens: HashMap<String, TokenConfig>,
    #[serde(default)]
    pub limits: LimitsConfig,
    /// HTTPS configuration (renamed from acme for clarity)
    #[serde(default, alias = "acme")]
    pub https: Option<HttpsConfig>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    /// Whether this token has admin privileges
    #[serde(default)]
    pub admin: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub domain: String,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_https_port")]
    pub https_port: u16,
}

const CONTROL_PATH: &str = "/_tunnel/connect";

impl ServerConfig {
    pub fn control_path(&self) -> &'static str {
        CONTROL_PATH
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpsConfig {
    pub email: String,
    #[serde(default = "default_acme_directory")]
    pub directory: String,
    #[serde(default = "default_certs_dir")]
    pub certs_dir: String,
    #[serde(default)]
    pub staging: bool,
    /// Path to additional root CA PEM file (for testing with Pebble)
    pub ca_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LimitsConfig {
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_max_body")]
    pub max_request_body_bytes: usize,
    #[serde(default = "default_idle_timeout")]
    pub idle_tunnel_timeout_secs: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            request_timeout_secs: default_request_timeout(),
            max_request_body_bytes: default_max_body(),
            idle_tunnel_timeout_secs: default_idle_timeout(),
        }
    }
}

fn default_http_port() -> u16 {
    80
}
fn default_https_port() -> u16 {
    443
}
fn default_request_timeout() -> u64 {
    30
}
fn default_max_body() -> usize {
    10 * 1024 * 1024
}
fn default_idle_timeout() -> u64 {
    3600
}
fn default_acme_directory() -> String {
    "https://acme-v02.api.letsencrypt.org/directory".to_string()
}
fn default_certs_dir() -> String {
    "/var/lib/loophole/certs".to_string()
}

impl Config {
    /// Load configuration from file
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;

        if config.version != CONFIG_VERSION {
            anyhow::bail!(
                "Config file version {} is not supported (expected {}). Please regenerate with 'loophole init'.",
                config.version,
                CONFIG_VERSION
            );
        }

        Ok(config)
    }

    /// Load configuration from environment variables (for Docker deployments)
    pub fn from_env() -> anyhow::Result<Self> {
        let domain = std::env::var(env::DOMAIN)
            .map_err(|_| anyhow::anyhow!("{} environment variable is required", env::DOMAIN))?;

        let http_port = std::env::var(env::HTTP_PORT)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_http_port);

        let https_port = std::env::var(env::HTTPS_PORT)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_https_port);

        // Parse tokens from comma-separated list
        let tokens_str = std::env::var(env::TOKENS)
            .map_err(|_| anyhow::anyhow!("{} environment variable is required", env::TOKENS))?;

        let mut tokens: HashMap<String, TokenConfig> = tokens_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|token| (token, TokenConfig { admin: false }))
            .collect();

        // Add admin tokens if specified
        if let Ok(admin_tokens_str) = std::env::var(env::ADMIN_TOKENS) {
            for token in admin_tokens_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                tokens.insert(token, TokenConfig { admin: true });
            }
        }

        if tokens.is_empty() {
            anyhow::bail!("{} must contain at least one token", env::TOKENS);
        }

        // Parse HTTPS/ACME config if email is provided
        let https = std::env::var(env::ACME_EMAIL).ok().map(|email| {
            let staging = std::env::var(env::ACME_STAGING)
                .ok()
                .map(|s| s.eq_ignore_ascii_case("true") || s == "1")
                .unwrap_or(false);

            let directory = std::env::var(env::ACME_DIRECTORY)
                .ok()
                .unwrap_or_else(default_acme_directory);

            let certs_dir = std::env::var(env::CERTS_DIR)
                .ok()
                .unwrap_or_else(default_certs_dir);

            HttpsConfig {
                email,
                directory,
                certs_dir,
                staging,
                ca_file: None,
            }
        });

        // Parse limits
        let request_timeout_secs = std::env::var(env::REQUEST_TIMEOUT)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_request_timeout);

        let max_request_body_bytes = std::env::var(env::MAX_BODY)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_max_body);

        let idle_tunnel_timeout_secs = std::env::var(env::IDLE_TIMEOUT)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_idle_timeout);

        Ok(Config {
            version: CONFIG_VERSION,
            server: ServerConfig {
                domain,
                http_port,
                https_port,
            },
            tokens,
            limits: LimitsConfig {
                request_timeout_secs,
                max_request_body_bytes,
                idle_tunnel_timeout_secs,
            },
            https,
        })
    }

    /// Load configuration: try file first, fall back to environment variables
    pub fn load_or_from_env(path: Option<&str>) -> anyhow::Result<Self> {
        // If a specific path is provided, try to load from file
        if let Some(path) = path {
            if Path::new(path).exists() {
                return Self::load(path);
            }
        }

        // Check if LOOPHOLE_DOMAIN is set (indicates env-based config)
        if std::env::var(env::DOMAIN).is_ok() {
            return Self::from_env();
        }

        // If we had a path, give a helpful error
        if let Some(path) = path {
            anyhow::bail!(
                "Config file '{}' not found and {} not set. Either create a config file or set environment variables.",
                path,
                env::DOMAIN
            );
        }

        anyhow::bail!(
            "No configuration found. Set {} and {} environment variables, or provide a config file path.",
            env::DOMAIN,
            env::TOKENS
        );
    }

    /// Validate a token and return its config if valid
    pub fn validate_token(&self, token: &str) -> Option<&TokenConfig> {
        self.tokens.get(token)
    }

    /// Check if a token is valid and has admin privileges
    pub fn validate_admin_token(&self, token: &str) -> bool {
        self.tokens
            .get(token)
            .map(|t| t.admin)
            .unwrap_or(false)
    }
}
