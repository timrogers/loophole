use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub tokens: HashMap<String, u32>,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub acme: Option<AcmeConfig>,
    #[serde(default)]
    pub admin: Option<AdminConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub domain: String,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_https_port")]
    pub https_port: u16,
    #[serde(default = "default_control_path")]
    pub control_path: String,
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
pub struct AdminConfig {
    #[serde(default)]
    pub enabled: bool,
    pub token: String,
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
    8080
}
fn default_https_port() -> u16 {
    8443
}
fn default_control_path() -> String {
    "/_tunnel/connect".to_string()
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
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn validate_token(&self, token: &str) -> Option<u32> {
        self.tokens.get(token).copied()
    }
}
