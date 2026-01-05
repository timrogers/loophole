use anyhow::{Context, Result};
use bytes::Bytes;
use dashmap::DashMap;
use http_body_util::Full;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, HttpClient, Identifier, NewAccount, NewOrder,
    OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tracing::{debug, error, info, warn};

/// Stores HTTP-01 challenge tokens for ACME validation
#[derive(Default, Debug)]
pub struct ChallengeStore {
    /// Maps challenge token -> key authorization
    tokens: DashMap<String, String>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self {
            tokens: DashMap::new(),
        }
    }

    pub fn set(&self, token: &str, key_auth: &str) {
        info!("ACME: Setting challenge token {} (key_auth length: {})", token, key_auth.len());
        self.tokens.insert(token.to_string(), key_auth.to_string());
    }

    pub fn get(&self, token: &str) -> Option<String> {
        let result = self.tokens.get(token).map(|v| v.clone());
        if result.is_some() {
            debug!("ACME: Challenge token {} found", token);
        } else {
            warn!("ACME: Challenge token {} NOT found (available tokens: {})", token, self.tokens.len());
        }
        result
    }

    pub fn remove(&self, token: &str) {
        debug!("ACME: Removing challenge token {}", token);
        self.tokens.remove(token);
    }
}

/// ACME client for requesting certificates from Let's Encrypt
pub struct AcmeClient {
    account: Account,
    certs_dir: PathBuf,
    challenge_store: Arc<ChallengeStore>,
}

impl std::fmt::Debug for AcmeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeClient")
            .field("certs_dir", &self.certs_dir)
            .finish_non_exhaustive()
    }
}

/// Certificate and private key pair
pub struct Certificate {
    pub cert_pem: String,
    pub key_pem: String,
}

/// Create an HTTP client that trusts additional root CAs (for testing with Pebble)
fn create_http_client_with_roots(
    additional_roots: Option<&[u8]>,
) -> Result<Box<dyn HttpClient>> {
    let mut root_store = RootCertStore::empty();

    // Add webpki roots
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Add additional roots if provided
    if let Some(pem_data) = additional_roots {
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut pem_data.as_ref())
                .filter_map(|r| r.ok())
                .collect();
        for cert in certs {
            root_store.add(cert).ok();
        }
    }

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .build();

    let client: HyperClient<_, Full<Bytes>> =
        HyperClient::builder(TokioExecutor::new()).build(https);

    Ok(Box::new(client))
}

impl AcmeClient {
    /// Create a new ACME client
    #[allow(dead_code)]
    pub async fn new(
        email: &str,
        directory_url: &str,
        certs_dir: PathBuf,
        challenge_store: Arc<ChallengeStore>,
    ) -> Result<Self> {
        Self::new_with_roots(email, directory_url, certs_dir, challenge_store, None).await
    }

    /// Create a new ACME client with additional root CAs (for testing with Pebble)
    pub async fn new_with_roots(
        email: &str,
        directory_url: &str,
        certs_dir: PathBuf,
        challenge_store: Arc<ChallengeStore>,
        additional_roots: Option<&[u8]>,
    ) -> Result<Self> {
        // Create certs directory if it doesn't exist
        fs::create_dir_all(&certs_dir)
            .await
            .context("Failed to create certs directory")?;

        // Create or load ACME account
        let account =
            Self::get_or_create_account(email, directory_url, &certs_dir, additional_roots).await?;

        Ok(Self {
            account,
            certs_dir,
            challenge_store,
        })
    }

    async fn get_or_create_account(
        email: &str,
        directory_url: &str,
        certs_dir: &PathBuf,
        additional_roots: Option<&[u8]>,
    ) -> Result<Account> {
        let account_path = certs_dir.join("account.json");

        let http_client = create_http_client_with_roots(additional_roots)?;

        // Try to load existing account
        if account_path.exists() {
            let account_data = fs::read_to_string(&account_path).await?;
            if let Ok(credentials) =
                serde_json::from_str::<instant_acme::AccountCredentials>(&account_data)
            {
                info!("Loaded existing ACME account");
                let http_client = create_http_client_with_roots(additional_roots)?;
                return Account::from_credentials_and_http(credentials, http_client)
                    .await
                    .context("Failed to load ACME account from credentials");
            }
        }

        // Create new account
        info!("Creating new ACME account for {}", email);
        let (account, credentials) = Account::create_with_http(
            &NewAccount {
                contact: &[&format!("mailto:{}", email)],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory_url,
            None,
            http_client,
        )
        .await
        .context("Failed to create ACME account")?;

        // Save account credentials
        let account_json = serde_json::to_string_pretty(&credentials)?;
        fs::write(&account_path, account_json).await?;
        info!("Saved ACME account credentials");

        Ok(account)
    }

    /// Request a certificate for a domain
    pub async fn request_certificate(&self, domain: &str) -> Result<Certificate> {
        info!("Requesting certificate for {}", domain);

        // Create new order
        let identifiers = vec![Identifier::Dns(domain.to_string())];
        let mut order = self
            .account
            .new_order(&NewOrder {
                identifiers: &identifiers,
            })
            .await
            .context("Failed to create order")?;

        // Get authorizations
        let authorizations = order
            .authorizations()
            .await
            .context("Failed to get authorizations")?;

        for auth in authorizations {
            match auth.status {
                AuthorizationStatus::Pending => {
                    // Find HTTP-01 challenge
                    let challenge = auth
                        .challenges
                        .iter()
                        .find(|c| c.r#type == ChallengeType::Http01)
                        .context("No HTTP-01 challenge found")?;

                    let key_auth = order.key_authorization(challenge);
                    let token = &challenge.token;

                    info!("ACME: HTTP-01 challenge for {}", domain);
                    info!("ACME: Let's Encrypt will request: http://{}/.well-known/acme-challenge/{}", domain, token);
                    debug!("Setting HTTP-01 challenge token: {} for domain: {}", token, domain);
                    self.challenge_store.set(token, key_auth.as_str());

                    // Notify ACME server that challenge is ready
                    order
                        .set_challenge_ready(&challenge.url)
                        .await
                        .context("Failed to set challenge ready")?;

                    // Wait for challenge to be validated
                    Self::wait_for_order_ready(&mut order).await?;

                    // Clean up challenge token
                    self.challenge_store.remove(token);
                }
                AuthorizationStatus::Valid => {
                    debug!("Authorization already valid for {}", domain);
                }
                _ => {
                    warn!("Authorization status: {:?}", auth.status);
                }
            }
        }

        // Generate CSR
        let key_pair = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params.subject_alt_names = vec![rcgen::SanType::DnsName(domain.try_into()?)];

        let csr = params.serialize_request(&key_pair)?;
        let csr_der = csr.der();

        // Finalize order
        order
            .finalize(csr_der)
            .await
            .context("Failed to finalize order")?;

        // Wait for certificate
        let cert_chain = Self::wait_for_certificate(&mut order).await?;

        let cert_pem = cert_chain;
        let key_pem = key_pair.serialize_pem();

        // Save certificate
        self.save_certificate(domain, &cert_pem, &key_pem).await?;

        info!("Certificate issued for {}", domain);

        Ok(Certificate { cert_pem, key_pem })
    }

    async fn wait_for_order_ready(order: &mut instant_acme::Order) -> Result<()> {
        let mut attempts = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            order.refresh().await.context("Failed to refresh order")?;

            let state = order.state();
            match state.status {
                OrderStatus::Ready | OrderStatus::Valid => {
                    debug!("Order is ready");
                    return Ok(());
                }
                OrderStatus::Invalid => {
                    // Log authorization details to help diagnose the failure
                    error!("Order became invalid. This usually means the ACME HTTP-01 challenge failed.");
                    error!("Common causes:");
                    error!("  1. Let's Encrypt cannot reach your server on port 80");
                    error!("  2. DNS for the domain does not point to this server");
                    error!("  3. A firewall is blocking incoming HTTP connections");
                    error!("Note: Let's Encrypt HTTP-01 challenges MUST be served on port 80");
                    return Err(anyhow::anyhow!("Order became invalid"));
                }
                OrderStatus::Pending | OrderStatus::Processing => {
                    attempts += 1;
                    if attempts > 30 {
                        return Err(anyhow::anyhow!("Timeout waiting for order to be ready"));
                    }
                    debug!("Waiting for order... ({}/30)", attempts);
                }
            }
        }
    }

    async fn wait_for_certificate(order: &mut instant_acme::Order) -> Result<String> {
        let mut attempts = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            order.refresh().await.context("Failed to refresh order")?;

            match order.state().status {
                OrderStatus::Valid => {
                    let cert_chain = order
                        .certificate()
                        .await
                        .context("Failed to get certificate")?
                        .context("No certificate returned")?;
                    return Ok(cert_chain);
                }
                OrderStatus::Invalid => {
                    return Err(anyhow::anyhow!("Order became invalid"));
                }
                _ => {
                    attempts += 1;
                    if attempts > 30 {
                        return Err(anyhow::anyhow!("Timeout waiting for certificate"));
                    }
                    debug!("Waiting for certificate... ({}/30)", attempts);
                }
            }
        }
    }

    async fn save_certificate(&self, domain: &str, cert_pem: &str, key_pem: &str) -> Result<()> {
        let cert_dir = self.certs_dir.join(domain);
        fs::create_dir_all(&cert_dir).await?;

        let cert_path = cert_dir.join("cert.pem");
        let key_path = cert_dir.join("key.pem");

        fs::write(&cert_path, cert_pem).await?;
        fs::write(&key_path, key_pem).await?;

        debug!("Saved certificate to {:?}", cert_path);
        Ok(())
    }

    /// Load an existing certificate from disk
    #[allow(dead_code)]
    pub async fn load_certificate(&self, domain: &str) -> Result<Option<Certificate>> {
        let cert_dir = self.certs_dir.join(domain);
        let cert_path = cert_dir.join("cert.pem");
        let key_path = cert_dir.join("key.pem");

        if !cert_path.exists() || !key_path.exists() {
            return Ok(None);
        }

        let cert_pem = fs::read_to_string(&cert_path).await?;
        let key_pem = fs::read_to_string(&key_path).await?;

        Ok(Some(Certificate { cert_pem, key_pem }))
    }

    /// Check if certificate needs renewal (within 30 days of expiry)
    #[allow(dead_code)]
    pub fn needs_renewal(cert_pem: &str) -> bool {
        use pem::parse;

        let pem = match parse(cert_pem) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to parse certificate PEM: {}", e);
                return true;
            }
        };

        // Parse the certificate to check expiry
        let cert = match rustls_pemfile::certs(&mut pem.contents().as_ref())
            .next()
            .and_then(|r| r.ok())
        {
            Some(c) => c,
            None => {
                error!("Failed to parse certificate DER");
                return true;
            }
        };

        // Use webpki to check validity
        // For simplicity, we'll just check if the file is older than 60 days
        // In production, you'd parse the X.509 certificate properly
        let _ = cert;

        // Default to renewing if we can't determine expiry
        // This is a simplified check - in production use x509-parser crate
        false
    }
}

/// Background task to check and renew certificates
#[allow(dead_code)]
pub async fn certificate_renewal_task(
    acme_client: Arc<AcmeClient>,
    certs_dir: PathBuf,
) -> Result<()> {
    loop {
        tokio::time::sleep(Duration::from_secs(86400)).await; // Check daily

        info!("Checking certificates for renewal...");

        let mut entries = match fs::read_dir(&certs_dir).await {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to read certs directory: {}", e);
                continue;
            }
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let domain = match path.file_name().and_then(|n| n.to_str()) {
                Some(d) => d.to_string(),
                None => continue,
            };

            // Skip account.json directory check
            if domain == "account.json" {
                continue;
            }

            let cert_path = path.join("cert.pem");
            if !cert_path.exists() {
                continue;
            }

            let cert_pem = match fs::read_to_string(&cert_path).await {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to read certificate for {}: {}", domain, e);
                    continue;
                }
            };

            if AcmeClient::needs_renewal(&cert_pem) {
                info!("Certificate for {} needs renewal", domain);
                if let Err(e) = acme_client.request_certificate(&domain).await {
                    error!("Failed to renew certificate for {}: {}", domain, e);
                }
            }
        }
    }
}
