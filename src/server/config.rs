use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    pub server: ServerConfig,
    pub tokens: HashMap<String, TokenConfig>,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub acme: Option<AcmeConfig>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    /// Maximum number of tunnels this token can create (0 = unlimited)
    #[serde(default)]
    pub max_tunnels: u32,
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
pub struct AcmeConfig {
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
