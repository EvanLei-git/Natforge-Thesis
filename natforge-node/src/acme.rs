//! Automatic per-domain HTTPS for custom domains via ACME (Let's Encrypt, HTTP-01).
//!
//! A custom `http`-route domain (e.g. play.mygame.com) is not covered by the
//! `*.<apex>` wildcard, so we obtain a per-domain certificate from an ACME CA. The
//! HTTP-01 challenge is served by the `:80` router (which reads the token response
//! out of the shared `challenges` map), and the `:443` router terminates TLS for
//! these domains using the `CertStore` resolver below.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::Context as _;
use instant_acme::{
    Account, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, RetryPolicy,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

/// Shared map of ACME HTTP-01 tokens to their key-authorization responses, served
/// by the `:80` router at `/.well-known/acme-challenge/<token>`.
pub type Challenges = Arc<RwLock<HashMap<String, String>>>;

/// A rustls cert resolver holding per-custom-domain certificates, keyed by SNI
/// hostname. Sync locks because `resolve` is a synchronous trait method.
#[derive(Debug, Default)]
pub struct CertStore {
    certs: StdRwLock<HashMap<String, Arc<CertifiedKey>>>,
}

impl CertStore {
    pub fn has(&self, domain: &str) -> bool {
        self.certs.read().unwrap().contains_key(domain)
    }
    pub fn insert(&self, domain: String, ck: Arc<CertifiedKey>) {
        self.certs.write().unwrap().insert(domain, ck);
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = client_hello.server_name()?;
        self.certs.read().unwrap().get(name).cloned()
    }
}

/// Build a rustls `CertifiedKey` from a PEM cert chain + private key.
pub fn certified_key(cert_pem: &str, key_pem: &str) -> anyhow::Result<Arc<CertifiedKey>> {
    let certs = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .context("parse ACME cert chain")?;
    anyhow::ensure!(!certs.is_empty(), "empty ACME cert chain");
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).context("parse ACME key")?;
    let signing_key =
        rustls::crypto::ring::sign::any_supported_type(&key).context("load ACME signing key")?;
    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
}

/// Create `dir` (recursively) restricted to the owner (0700) on Unix.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Write `data` to `path` restricted to the owner (0600) on Unix. Used for private
/// keys, which must never be world-readable on a shared host.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}

/// Obtain a certificate for `domain` from the ACME CA at `directory_url` via the
/// HTTP-01 challenge, publishing the challenge response through `challenges` for the
/// `:80` router to serve. Returns the (cert_chain_pem, private_key_pem).
pub async fn issue_certificate(
    directory_url: &str,
    email: &str,
    domain: &str,
    challenges: &Challenges,
) -> anyhow::Result<(String, String)> {
    let builder = Account::builder().context("ACME account builder")?;
    let contact = format!("mailto:{email}");
    let contacts: Vec<&str> = if email.is_empty() {
        vec![]
    } else {
        vec![contact.as_str()]
    };
    let (account, _creds) = builder
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory_url.to_string(),
            None,
        )
        .await
        .context("ACME account create")?;

    let identifiers = [Identifier::Dns(domain.to_string())];
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .context("ACME new order")?;

    // Publish each HTTP-01 challenge response and tell the CA we're ready.
    let mut tokens = Vec::new();
    {
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authz = result?;
            if let Some(mut challenge) = authz.challenge(ChallengeType::Http01) {
                let key_auth = challenge.key_authorization();
                let token = challenge.token.clone();
                challenges
                    .write()
                    .await
                    .insert(token.clone(), key_auth.as_str().to_string());
                tokens.push(token);
                challenge.set_ready().await?;
            }
        }
    } // drop `authorizations` to release the &mut borrow of `order`

    let result = async {
        order.poll_ready(&RetryPolicy::default()).await?;
        let key_pem = order.finalize().await?;
        // The order moves processing -> valid; poll for the issued chain.
        let mut cert_pem = None;
        for _ in 0..30 {
            match order.certificate().await? {
                Some(c) => {
                    cert_pem = Some(c);
                    break;
                }
                None => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        }
        let cert_pem =
            cert_pem.ok_or_else(|| anyhow::anyhow!("certificate not ready after finalize"))?;
        anyhow::Ok((cert_pem, key_pem))
    }
    .await;

    // Drop the challenge tokens whether issuance succeeded or not.
    {
        let mut c = challenges.write().await;
        for t in &tokens {
            c.remove(t);
        }
    }

    let (cert_pem, key_pem) = result.context("ACME order")?;
    info!("issued ACME certificate for '{domain}'");
    Ok((cert_pem, key_pem))
}

/// Runtime state for automatic per-domain HTTPS on custom domains. Holds the cert
/// resolver (and the `:443` acceptor built from it), the HTTP-01 challenge map the
/// `:80` router serves, and the ACME account settings. Disabled unless configured.
pub struct AcmeState {
    pub enabled: bool,
    directory_url: String,
    email: String,
    dir: PathBuf,
    store: Arc<CertStore>,
    challenges: Challenges,
    acceptor: TlsAcceptor,
    /// Domains with an issuance in flight (so concurrent triggers order only once).
    issuing: tokio::sync::Mutex<std::collections::HashSet<String>>,
}

impl AcmeState {
    pub fn new(
        enabled: bool,
        staging: bool,
        directory_override: Option<String>,
        email: String,
        dir: impl Into<PathBuf>,
    ) -> anyhow::Result<Arc<Self>> {
        let directory_url = directory_override.unwrap_or_else(|| {
            if staging {
                LetsEncrypt::Staging.url().to_string()
            } else {
                LetsEncrypt::Production.url().to_string()
            }
        });
        let store = Arc::new(CertStore::default());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("acme tls versions")?
            .with_no_client_auth()
            .with_cert_resolver(store.clone());
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let state = Arc::new(Self {
            enabled,
            directory_url,
            email,
            dir: dir.into(),
            store,
            challenges: Arc::new(RwLock::new(HashMap::new())),
            acceptor,
            issuing: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        });
        // Ensure the credential directory is owner-only if it already exists.
        #[cfg(unix)]
        if state.dir.exists() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&state.dir, std::fs::Permissions::from_mode(0o700));
        }
        state.load_existing();
        if enabled {
            info!(
                "ACME auto-HTTPS enabled (directory: {})",
                state.directory_url
            );
        }
        Ok(state)
    }

    /// The `:443` acceptor whose resolver serves per-domain certs by SNI.
    pub fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }
    pub fn has_cert(&self, domain: &str) -> bool {
        self.store.has(domain)
    }
    /// The HTTP-01 key-authorization for `token`, if a challenge is in progress.
    pub async fn challenge_response(&self, token: &str) -> Option<String> {
        self.challenges.read().await.get(token).cloned()
    }

    /// Load previously-issued certs from disk into the resolver on boot.
    fn load_existing(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(domain) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if let (Ok(c), Ok(k)) = (
                std::fs::read_to_string(path.join("cert.pem")),
                std::fs::read_to_string(path.join("key.pem")),
            ) {
                match certified_key(&c, &k) {
                    Ok(ck) => {
                        self.store.insert(domain.clone(), ck);
                        info!("loaded ACME cert for '{domain}'");
                    }
                    Err(e) => warn!("ignoring bad stored ACME cert for '{domain}': {e}"),
                }
            }
        }
    }

    /// Ensure a certificate exists for `domain`, issuing one in the background if
    /// not. Idempotent: concurrent triggers for the same domain issue only once.
    pub fn ensure_cert(self: &Arc<Self>, domain: String) {
        if !self.enabled || self.store.has(&domain) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            {
                let mut inflight = this.issuing.lock().await;
                if this.store.has(&domain) || inflight.contains(&domain) {
                    return;
                }
                inflight.insert(domain.clone());
            }
            let outcome = this.issue_and_store(&domain).await;
            this.issuing.lock().await.remove(&domain);
            if let Err(e) = outcome {
                warn!("ACME issuance for '{domain}' failed (will retry on reconnect): {e}");
            }
        });
    }

    async fn issue_and_store(&self, domain: &str) -> anyhow::Result<()> {
        let (cert_pem, key_pem) =
            issue_certificate(&self.directory_url, &self.email, domain, &self.challenges).await?;
        let dir = self.dir.join(domain);
        create_private_dir(&dir).context("create acme cert dir")?;
        // cert.pem is public; key.pem is a private key and must be owner-only.
        std::fs::write(dir.join("cert.pem"), &cert_pem).context("write cert.pem")?;
        write_private(&dir.join("key.pem"), key_pem.as_bytes()).context("write key.pem")?;
        let ck = certified_key(&cert_pem, &key_pem)?;
        self.store.insert(domain.to_string(), ck);
        info!("ACME certificate active for '{domain}'");
        Ok(())
    }

    /// Re-issue certs whose files are older than ~60 days (LE certs last 90).
    pub async fn renew_due(self: &Arc<Self>) {
        if !self.enabled {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(domain) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let age_days = std::fs::metadata(path.join("cert.pem"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs() / 86400)
                .unwrap_or(0);
            if age_days >= 60 {
                info!("renewing ACME cert for '{domain}' ({age_days} days old)");
                if let Err(e) = self.issue_and_store(&domain).await {
                    warn!("ACME renewal for '{domain}' failed: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certified_key_and_store() {
        // A self-signed cert stands in for an issued ACME cert.
        let cert = rcgen::generate_simple_self_signed(vec!["play.test".to_string()]).unwrap();
        let ck = certified_key(&cert.cert.pem(), &cert.signing_key.serialize_pem()).unwrap();
        let store = CertStore::default();
        assert!(!store.has("play.test"));
        store.insert("play.test".to_string(), ck);
        assert!(store.has("play.test"));
    }
}
