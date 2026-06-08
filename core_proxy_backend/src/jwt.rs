//! Tunnel-token verification (stateless; uses only the shared HMAC secret).
//!
//! The claims struct (`natforge_proto::TunnelClaims`) is shared with the control
//! plane, so issue and verify can never disagree on shape.

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

pub use natforge_proto::{RouteClaim, TunnelClaims};

/// Verify a tunnel token and return its claims, or an error string.
pub fn verify_tunnel_token(token: &str, secret: &str) -> Result<TunnelClaims, String> {
    let validation = Validation::new(Algorithm::HS256); // requires `exp`
    let data = decode::<TunnelClaims>(token, &DecodingKey::from_secret(secret.as_bytes()), &validation)
        .map_err(|e| format!("invalid tunnel token: {e}"))?;
    data.claims.validate_shape()?;
    Ok(data.claims)
}
