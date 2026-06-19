//! Client-side TLS for the agent↔core control connection.
//!
//! The core proxy presents a self-signed certificate, so instead of a CA we
//! authenticate it by pinning the SHA-256 fingerprint the control plane handed
//! us with the tunnel reservation. The whole yamux session (and every user
//! connection multiplexed inside it) then rides this encrypted, pinned channel.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

/// A rustls verifier that accepts exactly one certificate — the one whose DER
/// SHA-256 matches the pinned fingerprint. Handshake signatures are still verified
/// against that certificate's key by the crypto provider.
#[derive(Debug)]
struct PinnedCert {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = fingerprint_of(end_entity);
        if got.eq_ignore_ascii_case(&self.fingerprint) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "control certificate fingerprint mismatch (pinned {}, presented {got})",
                self.fingerprint
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn fingerprint_of(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// TLS-wrap an established TCP stream to the core, pinning its cert fingerprint.
pub async fn connect(tcp: TcpStream, fingerprint: &str) -> Result<TlsStream<TcpStream>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("tls protocol versions: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert {
            fingerprint: fingerprint.to_string(),
            provider,
        }))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    // The pin authenticates the peer; the SNI name is cosmetic (cert SAN matches).
    let server_name = ServerName::try_from("natforge-core").map_err(|_| anyhow!("bad server name"))?;
    Ok(connector.connect(server_name, tcp).await?)
}
