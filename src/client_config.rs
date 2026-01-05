use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub version: u32,
    pub server: String,
    pub token: String,
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("loophole")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

impl ClientConfig {
    pub fn new(server: String, token: String) -> Self {
        Self {
            version: CONFIG_VERSION,
            server,
            token,
        }
    }

    pub fn load() -> Result<Option<Self>> {
        let path = config_path();
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .context(format!("Failed to read config from {}", path.display()))?;

        let config: ClientConfig = toml::from_str(&content)
            .context(format!("Failed to parse config from {}", path.display()))?;

        if config.version != CONFIG_VERSION {
            anyhow::bail!(
                "Config file version {} is not supported (expected {}). Please run 'loophole login' again.",
                config.version,
                CONFIG_VERSION
            );
        }

        Ok(Some(config))
    }

    pub fn save(&self) -> Result<PathBuf> {
        let dir = config_dir();
        fs::create_dir_all(&dir)
            .context(format!("Failed to create config directory {}", dir.display()))?;

        let path = config_path();
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&path, content)
            .context(format!("Failed to write config to {}", path.display()))?;

        Ok(path)
    }
}
