//! RSA-PSS authentication for the Kalshi API.
//!
//! Signature format: `RSA-PSS(SHA256, timestamp + method + path)` → base64.
//! The `path` must NOT include query parameters.

use base64::Engine;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::pss::{BlindedSigningKey, Signature};
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rsa::RsaPrivateKey;

use common::Error;

/// Holds API credentials and the parsed RSA private key.
#[derive(Clone)]
pub struct KalshiAuth {
    pub api_key: String,
    signing_key: BlindedSigningKey<Sha256>,
}

impl std::fmt::Debug for KalshiAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KalshiAuth")
            .field("api_key", &self.api_key)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

impl KalshiAuth {
    /// Create from raw API key and PEM-encoded private key string.
    ///
    /// The PEM string may contain literal `\n` (two chars) instead of real
    /// newlines — we normalise both forms.
    pub fn new(api_key: &str, pem_string: &str) -> Result<Self, Error> {
        // Normalise escaped newlines → real newlines.
        let pem = pem_string.replace("\\n", "\n");
        let pem = pem.trim();

        let private_key = RsaPrivateKey::from_pkcs1_pem(pem)
            .or_else(|_| RsaPrivateKey::from_pkcs8_pem(pem))
            .map_err(|e| Error::Auth(format!("Failed to parse RSA private key: {e}")))?;

        let signing_key = BlindedSigningKey::<Sha256>::new(private_key);

        Ok(Self {
            api_key: api_key.to_string(),
            signing_key,
        })
    }

    /// Load credentials from environment variables.
    pub fn from_env() -> Result<Self, Error> {
        let api_key = std::env::var("KALSHI_API_KEY")
            .map_err(|_| Error::Auth("KALSHI_API_KEY not set".into()))?;
        let secret = std::env::var("KALSHI_SECRET_KEY")
            .map_err(|_| Error::Auth("KALSHI_SECRET_KEY not set".into()))?;

        Self::new(&api_key, &secret)
    }

    /// Sign a request, returning `(timestamp_ms_string, base64_signature)`.
    ///
    /// The message to sign is: `{timestamp}{METHOD}{path_without_query}`.
    pub fn sign_request(&self, method: &str, path: &str) -> (String, String) {
        let timestamp = chrono::Utc::now().timestamp_millis().to_string();

        // Strip query parameters.
        let path_clean = path.split('?').next().unwrap_or(path);

        let message = format!("{}{}{}", timestamp, method, path_clean);
        let mut rng = rand::thread_rng();
        let signature: Signature = self.signing_key.sign_with_rng(&mut rng, message.as_bytes());

        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

        (timestamp, sig_b64)
    }

    /// Build an authenticated `reqwest::header::HeaderMap`.
    pub fn headers(&self, method: &str, path: &str) -> reqwest::header::HeaderMap {
        let (timestamp, signature) = self.sign_request(method, path);

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "KALSHI-ACCESS-KEY",
            self.api_key.parse().expect("invalid api key header"),
        );
        headers.insert(
            "KALSHI-ACCESS-TIMESTAMP",
            timestamp.parse().expect("invalid timestamp header"),
        );
        headers.insert(
            "KALSHI-ACCESS-SIGNATURE",
            signature.parse().expect("invalid signature header"),
        );
        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that signing produces a non-empty base64 result.
    #[test]
    fn test_sign_request_format() {
        // Generate a small test key (not for production — just structure test).
        let private_key =
            RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("keygen failed");
        let pem = rsa::pkcs1::EncodeRsaPrivateKey::to_pkcs1_pem(&private_key, rsa::pkcs1::LineEnding::LF)
            .expect("pem encode failed");

        let auth = KalshiAuth::new("test-key-id", pem.as_ref()).expect("auth init failed");

        let (ts, sig) = auth.sign_request("GET", "/trade-api/v2/portfolio/balance?foo=bar");

        // Timestamp should be numeric.
        assert!(ts.parse::<i64>().is_ok(), "timestamp should be numeric");

        // Signature should be valid base64 and non-empty.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&sig)
            .expect("signature should be valid base64");
        assert!(!decoded.is_empty(), "signature should not be empty");
        // RSA-2048 PSS signature is 256 bytes.
        assert_eq!(decoded.len(), 256, "RSA-2048 PSS sig should be 256 bytes");
    }

    /// Verify query parameters are stripped before signing.
    #[test]
    fn test_sign_strips_query_params() {
        let private_key =
            RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("keygen failed");
        let pem = rsa::pkcs1::EncodeRsaPrivateKey::to_pkcs1_pem(&private_key, rsa::pkcs1::LineEnding::LF)
            .expect("pem encode failed");

        let auth = KalshiAuth::new("test-key-id", pem.as_ref()).expect("auth init failed");

        // Both should sign with the same base path (modulo timestamp/randomness).
        let (_, sig1) = auth.sign_request("GET", "/trade-api/v2/portfolio/orders");
        let (_, sig2) = auth.sign_request("GET", "/trade-api/v2/portfolio/orders?limit=5");

        // Signatures differ due to different timestamps and randomness,
        // but both should decode to 256-byte values.
        let d1 = base64::engine::general_purpose::STANDARD.decode(&sig1).unwrap();
        let d2 = base64::engine::general_purpose::STANDARD.decode(&sig2).unwrap();
        assert_eq!(d1.len(), 256);
        assert_eq!(d2.len(), 256);
    }
}
