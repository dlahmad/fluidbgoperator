use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PLUGIN_AUTH_TOKEN_ENV: &str = "FLUIDBG_PLUGIN_AUTH_TOKEN";
pub const AUTHORIZATION_HEADER: &str = "authorization";
pub const PLUGIN_AUTH_ISSUER: &str = "fluidbg-operator";
pub const PLUGIN_AUTH_AUDIENCE: &str = "fluidbg-inception-plugin";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginAuthClaims {
    pub iss: String,
    pub aud: String,
    pub namespace: String,
    pub blue_green_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blue_green_uid: Option<String>,
    pub inception_point: String,
    pub plugin: String,
}

impl PluginAuthClaims {
    pub fn new(namespace: &str, blue_green_ref: &str, inception_point: &str, plugin: &str) -> Self {
        Self {
            iss: PLUGIN_AUTH_ISSUER.to_string(),
            aud: PLUGIN_AUTH_AUDIENCE.to_string(),
            namespace: namespace.to_string(),
            blue_green_ref: blue_green_ref.to_string(),
            blue_green_uid: None,
            inception_point: inception_point.to_string(),
            plugin: plugin.to_string(),
        }
    }

    pub fn new_with_uid(
        namespace: &str,
        blue_green_ref: &str,
        blue_green_uid: &str,
        inception_point: &str,
        plugin: &str,
    ) -> Self {
        Self {
            blue_green_uid: Some(blue_green_uid.to_string()),
            ..Self::new(namespace, blue_green_ref, inception_point, plugin)
        }
    }
}

#[derive(Deserialize, Serialize)]
struct JwtHeader {
    alg: String,
    typ: String,
}

pub fn auth_token_from_env() -> Option<String> {
    std::env::var(PLUGIN_AUTH_TOKEN_ENV)
        .ok()
        .filter(|token| !token.is_empty())
}

pub fn bearer_value(token: &str) -> String {
    format!("Bearer {token}")
}

pub fn bearer_token(header_value: &str) -> Option<&str> {
    header_value.strip_prefix("Bearer ")
}

pub fn bearer_matches(header_value: Option<&str>, expected_token: Option<&str>) -> bool {
    let Some(expected_token) = expected_token else {
        return true;
    };
    let Some(header_value) = header_value else {
        return false;
    };
    bearer_token(header_value)
        .map(|actual| constant_time_eq(actual.as_bytes(), expected_token.as_bytes()))
        .unwrap_or(false)
}

pub fn sign_plugin_auth_token(claims: &PluginAuthClaims, signing_key: &[u8]) -> Result<String> {
    if signing_key.is_empty() {
        bail!("plugin auth signing key must not be empty");
    }
    let header = JwtHeader {
        alg: "HS256".to_string(),
        typ: "JWT".to_string(),
    };
    let encoded_header = encode_json(&header)?;
    let encoded_claims = encode_json(claims)?;
    let signing_input = format!("{encoded_header}.{encoded_claims}");
    let signature = hmac_sha256(signing_key, signing_input.as_bytes());
    Ok(format!(
        "{}.{}",
        signing_input,
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

pub fn verify_plugin_auth_token(token: &str, signing_key: &[u8]) -> Result<PluginAuthClaims> {
    if signing_key.is_empty() {
        bail!("plugin auth signing key must not be empty");
    }
    let mut parts = token.split('.');
    let encoded_header = parts.next().ok_or_else(|| anyhow!("JWT header missing"))?;
    let encoded_claims = parts.next().ok_or_else(|| anyhow!("JWT claims missing"))?;
    let encoded_signature = parts
        .next()
        .ok_or_else(|| anyhow!("JWT signature missing"))?;
    if parts.next().is_some() {
        bail!("JWT contains too many segments");
    }

    let header: JwtHeader = decode_json(encoded_header).context("invalid JWT header")?;
    if header.alg != "HS256" || header.typ != "JWT" {
        bail!("unsupported JWT header");
    }
    let signing_input = format!("{encoded_header}.{encoded_claims}");
    let expected = hmac_sha256(signing_key, signing_input.as_bytes());
    let actual = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .context("invalid JWT signature encoding")?;
    if !constant_time_eq(&actual, &expected) {
        bail!("JWT signature mismatch");
    }
    let claims: PluginAuthClaims = decode_json(encoded_claims).context("invalid JWT claims")?;
    if claims.iss != PLUGIN_AUTH_ISSUER || claims.aud != PLUGIN_AUTH_AUDIENCE {
        bail!("JWT issuer or audience mismatch");
    }
    Ok(claims)
}

fn encode_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?))
}

fn decode_json<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(value)?)?)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_match_requires_expected_token() {
        assert!(bearer_matches(None, None));
        assert!(bearer_matches(Some("Bearer secret"), Some("secret")));
        assert!(!bearer_matches(Some("Bearer wrong"), Some("secret")));
        assert!(!bearer_matches(Some("secret"), Some("secret")));
        assert!(!bearer_matches(None, Some("secret")));
    }

    #[test]
    fn signed_plugin_token_round_trips_claims() {
        let claims = PluginAuthClaims::new("app", "orders", "incoming", "rabbitmq");
        let token = sign_plugin_auth_token(&claims, b"signing-key").unwrap();

        assert_eq!(
            verify_plugin_auth_token(&token, b"signing-key").unwrap(),
            claims
        );
        assert!(verify_plugin_auth_token(&token, b"wrong-key").is_err());
    }
}
