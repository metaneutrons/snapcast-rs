#![allow(dead_code, unused_imports)]
//! Authentication — JWT token generation and validation.

use anyhow::Result;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// JWT claims.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// Subject (client ID or username).
    sub: String,
    /// Expiration (Unix timestamp).
    exp: u64,
}

/// Auth configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Whether authentication is required.
    pub enabled: bool,
    /// Secret key for JWT signing/validation.
    pub secret: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            secret: "snapcast-default-secret-key-32bytes!".into(),
        }
    }
}

/// Validate an HTTP Authorization header. Returns Ok(subject) or Err.
pub fn validate_bearer(config: &AuthConfig, header: Option<&str>) -> Result<String> {
    if !config.enabled {
        return Ok("anonymous".into());
    }
    let header = header.ok_or_else(|| anyhow::anyhow!("missing Authorization header"))?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| anyhow::anyhow!("expected Bearer token"))?;
    validate_token(config, token)
}

/// Generate a JWT token for the given subject.
pub fn generate_token(config: &AuthConfig, subject: &str) -> Result<String> {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        + 86400; // 24 hours

    let claims = Claims {
        sub: subject.into(),
        exp,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.secret.as_bytes()),
    )?;
    tracing::info!(subject, "auth token generated");
    Ok(token)
}

/// Validate a JWT token. Returns the subject if valid.
pub fn validate_token(config: &AuthConfig, token: &str) -> Result<String> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| {
        tracing::warn!(error = %e, "auth token validation failed");
        e
    })?;
    Ok(data.claims.sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_roundtrip() {
        let config = AuthConfig {
            enabled: true,
            secret: "test-secret-must-be-32-bytes-long".into(),
        };
        let token = generate_token(&config, "user1").unwrap();
        let subject = validate_token(&config, &token).unwrap();
        assert_eq!(subject, "user1");
    }

    #[test]
    fn invalid_token() {
        let config = AuthConfig {
            enabled: true,
            secret: "test-secret-must-be-32-bytes-long".into(),
        };
        assert!(validate_token(&config, "garbage").is_err());
    }

    #[test]
    fn wrong_secret() {
        let config1 = AuthConfig {
            enabled: true,
            secret: "secret1-must-be-at-least-32bytes!".into(),
        };
        let config2 = AuthConfig {
            enabled: true,
            secret: "secret2-must-be-at-least-32bytes!".into(),
        };
        let token = generate_token(&config1, "user1").unwrap();
        assert!(validate_token(&config2, &token).is_err());
    }
}
