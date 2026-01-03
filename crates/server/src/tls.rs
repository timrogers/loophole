use anyhow::{Context, Result};
use dashmap::DashMap;
use rustls::pki_types::CertificateDer;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, error, info, warn};

use crate::acme::{AcmeClient, ChallengeStore};

/// Manages TLS certificates with dynamic loading based on SNI
#[derive(Debug)]
pub struct CertManager {
    certs_dir: PathBuf,
    /// Maps domain -> CertifiedKey
    certs: DashMap<String, Arc<CertifiedKey>>,
    /// Domains with pending certificate requests
    pending: DashMap<String, ()>,
    /// ACME client for requesting certificates
    acme_client: Option<Arc<AcmeClient>>,
    /// Challenge store for HTTP-01 challenges
    challenge_store: Arc<ChallengeStore>,
    /// Base domain for the server
    base_domain: String,
}

impl CertManager {
    pub async fn new(
        certs_dir: PathBuf,
        acme_client: Option<Arc<AcmeClient>>,
        challenge_store: Arc<ChallengeStore>,
        base_domain: String,
    ) -> Result<Self> {
        let manager = Self {
            certs_dir: certs_dir.clone(),
            certs: DashMap::new(),
            pending: DashMap::new(),
            acme_client,
            challenge_store,
            base_domain,
        };

        // Load existing certificates
        manager.load_existing_certs().await?;

        Ok(manager)
    }

    async fn load_existing_certs(&self) -> Result<()> {
        if !self.certs_dir.exists() {
            fs::create_dir_all(&self.certs_dir).await?;
            return Ok(());
        }

        let mut entries = fs::read_dir(&self.certs_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let domain = match path.file_name().and_then(|n| n.to_str()) {
                Some(d) => d.to_string(),
                None => continue,
            };

            let cert_path = path.join("cert.pem");
            let key_path = path.join("key.pem");

            if !cert_path.exists() || !key_path.exists() {
                continue;
            }

            match self.load_cert_from_files(&cert_path, &key_path).await {
                Ok(certified_key) => {
                    info!("Loaded certificate for {}", domain);
                    self.certs.insert(domain, Arc::new(certified_key));
                }
                Err(e) => {
                    warn!("Failed to load certificate for {}: {}", domain, e);
                }
            }
        }

        Ok(())
    }

    async fn load_cert_from_files(
        &self,
        cert_path: &PathBuf,
        key_path: &PathBuf,
    ) -> Result<CertifiedKey> {
        let cert_pem = fs::read_to_string(cert_path).await?;
        let key_pem = fs::read_to_string(key_path).await?;

        Self::parse_certificate(&cert_pem, &key_pem)
    }

    pub fn parse_certificate(cert_pem: &str, key_pem: &str) -> Result<CertifiedKey> {
        // Parse certificates
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut cert_pem.as_bytes())
                .filter_map(|r| r.ok())
                .collect();

        if certs.is_empty() {
            return Err(anyhow::anyhow!("No certificates found in PEM"));
        }

        // Parse private key
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .context("Failed to parse private key")?
            .context("No private key found in PEM")?;

        // Create signing key
        let signing_key = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key)
            .map_err(|e| anyhow::anyhow!("Failed to create signing key: {:?}", e))?;

        Ok(CertifiedKey::new(certs, signing_key))
    }

    /// Get certificate for a domain, requesting one if not available
    #[allow(dead_code)]
    pub fn get_cert(&self, domain: &str) -> Option<Arc<CertifiedKey>> {
        self.certs.get(domain).map(|r| r.clone())
    }

    /// Check if a certificate exists for a domain
    pub fn has_cert(&self, domain: &str) -> bool {
        self.certs.contains_key(domain)
    }

    /// Add a certificate for a domain
    #[allow(dead_code)]
    pub fn add_cert(&self, domain: &str, cert: CertifiedKey) {
        self.certs.insert(domain.to_string(), Arc::new(cert));
    }

    /// Request a certificate for a domain (async)
    pub async fn request_cert(&self, domain: &str) -> Result<()> {
        // Check if already pending
        if self.pending.contains_key(domain) {
            debug!("Certificate request already pending for {}", domain);
            return Ok(());
        }

        // Check if already have cert
        if self.certs.contains_key(domain) {
            debug!("Certificate already exists for {}", domain);
            return Ok(());
        }

        let acme_client = match &self.acme_client {
            Some(c) => c.clone(),
            None => {
                warn!("ACME not configured, cannot request certificate for {}", domain);
                return Ok(());
            }
        };

        // Mark as pending
        self.pending.insert(domain.to_string(), ());

        info!("Requesting certificate for {}", domain);

        let result = acme_client.request_certificate(domain).await;

        // Remove pending status
        self.pending.remove(domain);

        match result {
            Ok(cert) => {
                let certified_key = Self::parse_certificate(&cert.cert_pem, &cert.key_pem)?;
                self.certs.insert(domain.to_string(), Arc::new(certified_key));
                info!("Certificate installed for {}", domain);
                Ok(())
            }
            Err(e) => {
                error!("Failed to get certificate for {}: {}", domain, e);
                Err(e)
            }
        }
    }

    /// Check if a certificate request is pending
    #[allow(dead_code)]
    pub fn is_pending(&self, domain: &str) -> bool {
        self.pending.contains_key(domain)
    }

    /// Get the challenge store
    #[allow(dead_code)]
    pub fn challenge_store(&self) -> Arc<ChallengeStore> {
        self.challenge_store.clone()
    }

    /// Get base domain
    #[allow(dead_code)]
    pub fn base_domain(&self) -> &str {
        &self.base_domain
    }
}

/// Implements rustls ResolvesServerCert for SNI-based certificate selection
impl ResolvesServerCert for CertManager {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let server_name = client_hello.server_name()?;
        
        debug!("SNI resolution for: {}", server_name);

        // Try exact match first
        if let Some(cert) = self.certs.get(server_name) {
            return Some(cert.clone());
        }

        // Try wildcard match for subdomain.base_domain
        if server_name.ends_with(&format!(".{}", self.base_domain)) {
            // Check for base domain wildcard cert
            let wildcard = format!("*.{}", self.base_domain);
            if let Some(cert) = self.certs.get(&wildcard) {
                return Some(cert.clone());
            }

            // Check for base domain cert (some setups allow this)
            if let Some(cert) = self.certs.get(&self.base_domain) {
                return Some(cert.clone());
            }
        }

        // No cert found - will trigger certificate request
        debug!("No certificate found for {}", server_name);
        None
    }
}

/// Create a rustls ServerConfig with the CertManager
pub fn create_tls_config(cert_manager: Arc<CertManager>) -> Result<rustls::ServerConfig> {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(cert_manager);

    Ok(config)
}
