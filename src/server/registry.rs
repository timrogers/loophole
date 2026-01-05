use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::Arc;
use thiserror::Error;

use super::tunnel::Tunnel;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("Subdomain is already taken")]
    SubdomainTaken,
    #[error("Invalid subdomain: {0}")]
    InvalidSubdomain(String),
    #[error("Reserved subdomain")]
    ReservedSubdomain,
}

pub struct Registry {
    tunnels: DashMap<String, Arc<Tunnel>>,
    reserved: HashSet<String>,
}

impl Registry {
    pub fn new() -> Self {
        let reserved: HashSet<String> = ["www", "api", "admin", "mail", "ftp", "ssh", "tunnel"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        Self {
            tunnels: DashMap::new(),
            reserved,
        }
    }

    pub fn validate_subdomain(subdomain: &str) -> Result<(), RegistryError> {
        if subdomain.len() < 3 || subdomain.len() > 63 {
            return Err(RegistryError::InvalidSubdomain(
                "Subdomain must be 3-63 characters".to_string(),
            ));
        }

        if !subdomain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(RegistryError::InvalidSubdomain(
                "Subdomain must contain only alphanumeric characters and hyphens".to_string(),
            ));
        }

        if subdomain.starts_with('-') || subdomain.ends_with('-') {
            return Err(RegistryError::InvalidSubdomain(
                "Subdomain cannot start or end with a hyphen".to_string(),
            ));
        }

        Ok(())
    }

    pub fn register(&self, subdomain: &str, tunnel: Arc<Tunnel>) -> Result<(), RegistryError> {
        Self::validate_subdomain(subdomain)?;

        if self.reserved.contains(subdomain) {
            return Err(RegistryError::ReservedSubdomain);
        }

        // Try to insert, fail if already exists
        match self.tunnels.entry(subdomain.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(RegistryError::SubdomainTaken),
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(tunnel);
                Ok(())
            }
        }
    }

    pub fn deregister(&self, subdomain: &str) {
        self.tunnels.remove(subdomain);
    }

    pub fn get(&self, subdomain: &str) -> Option<Arc<Tunnel>> {
        self.tunnels.get(subdomain).map(|r| r.value().clone())
    }

    /// Get all subdomain names (for iteration during idle cleanup)
    pub fn subdomains(&self) -> Vec<String> {
        self.tunnels.iter().map(|r| r.key().clone()).collect()
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.tunnels.len()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subdomain_validation() {
        assert!(Registry::validate_subdomain("myapp").is_ok());
        assert!(Registry::validate_subdomain("my-app").is_ok());
        assert!(Registry::validate_subdomain("app123").is_ok());

        assert!(Registry::validate_subdomain("ab").is_err()); // too short
        assert!(Registry::validate_subdomain("-myapp").is_err()); // starts with hyphen
        assert!(Registry::validate_subdomain("myapp-").is_err()); // ends with hyphen
        assert!(Registry::validate_subdomain("my_app").is_err()); // underscore
        assert!(Registry::validate_subdomain("my.app").is_err()); // dot
    }
}
