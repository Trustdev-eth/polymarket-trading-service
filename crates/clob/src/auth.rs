use base64::engine::general_purpose::URL_SAFE;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::eip712::hex0x;
use crate::sign::{sign_clob_auth, V2Signer};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct ApiCreds {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

/// L1 headers. Domain version stays `"1"` — unchanged in CLOB V2.
pub fn l1_headers(signer: &V2Signer, timestamp: u64, nonce: u64) -> Result<Vec<(String, String)>, crate::sign::SignError> {
    let sig = sign_clob_auth(signer, timestamp, nonce)?;
    Ok(vec![
        ("POLY_ADDRESS".into(), signer.address_hex()),
        ("POLY_SIGNATURE".into(), sig),
        ("POLY_TIMESTAMP".into(), timestamp.to_string()),
        ("POLY_NONCE".into(), nonce.to_string()),
    ])
}

/// L2 HMAC. Builder HMAC headers are gone in V2 (builder sits on the order).
pub fn l2_headers(
    address_hex: String,
    creds: &ApiCreds,
    timestamp: u64,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Vec<(String, String)> {
    let hmac = build_hmac(&creds.secret, timestamp, method, path, body);
    vec![
        ("POLY_ADDRESS".into(), address_hex),
        ("POLY_SIGNATURE".into(), hmac),
        ("POLY_TIMESTAMP".into(), timestamp.to_string()),
        ("POLY_API_KEY".into(), creds.api_key.clone()),
        ("POLY_PASSPHRASE".into(), creds.passphrase.clone()),
    ]
}

pub fn build_hmac(secret: &str, timestamp: u64, method: &str, path: &str, body: Option<&str>) -> String {
    let mut padded = secret.to_string();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    let key = URL_SAFE.decode(padded.as_bytes()).unwrap_or_default();
    let mut msg = format!("{timestamp}{method}{path}");
    if let Some(b) = body {
        msg.push_str(&b.replace('\'', "\""));
    }
    let mut mac = HmacSha256::new_from_slice(&key).expect("hmac key");
    mac.update(msg.as_bytes());
    URL_SAFE.encode(mac.finalize().into_bytes())
}

pub fn content_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), "mm-bot/0.1".into()),
        ("Accept".into(), "*/*".into()),
        ("Content-Type".into(), "application/json".into()),
    ]
}

pub fn address_header(addr: &[u8; 20]) -> String {
    hex0x(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_stable() {
        // secret is url-safe b64 of 32 zero bytes
        let secret = URL_SAFE.encode([0u8; 32]);
        let a = build_hmac(&secret, 1713398400, "POST", "/orders", Some("[{\"a\":1}]"));
        let b = build_hmac(&secret, 1713398400, "POST", "/orders", Some("[{\"a\":1}]"));
        assert_eq!(a, b);
        assert_ne!(
            a,
            build_hmac(&secret, 1713398400, "DELETE", "/orders", Some("[{\"a\":1}]"))
        );
    }
}
