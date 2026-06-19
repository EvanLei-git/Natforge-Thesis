//! TLS for the agent↔core control connection.
//!
//! This replaces the previous *simulated* WireGuard peer: the multiplexed tunnel
//! (yamux) now runs inside a real TLS 1.2/1.3 session. The core presents a
//! self-signed certificate generated at boot; the agent pins its SHA-256
//! fingerprint (delivered with the tunnel reservation), so the channel is
//! authenticated + encrypted end-to-end without operating a CA/PKI.

use std::sync::Arc;

use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;

/// A freshly generated self-signed identity for this node's control listener.
pub struct ControlTls {
    pub acceptor: TlsAcceptor,
    /// Lowercase-hex SHA-256 of the certificate DER (what the agent pins).
    pub fingerprint: String,
}

/// Generate a self-signed cert/key and wrap it into a TLS acceptor + fingerprint.
pub fn generate() -> anyhow::Result<ControlTls> {
    let cert = rcgen::generate_simple_self_signed(vec!["natforge-core".to_string()])
        .context("generate self-signed certificate")?;
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let fingerprint = fingerprint_of(&cert_der);

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls protocol versions")?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], PrivateKeyDer::Pkcs8(key_der))
        .context("install self-signed cert")?;
    config.alpn_protocols = vec![b"natforge/1".to_vec()];

    Ok(ControlTls { acceptor: TlsAcceptor::from(Arc::new(config)), fingerprint })
}

/// Lowercase-hex SHA-256 of a certificate's DER encoding.
pub fn fingerprint_of(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
