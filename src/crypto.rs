use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Result};
use rand::RngCore;

/// `KEY_WRAP_SECRET` must be 32 bytes as 64 hex chars (`openssl rand -hex 32`).
/// Never SHA-256 of a password — that is brute-forceable against a DB dump.
pub fn wrap_key(secret: &str) -> Result<[u8; 32]> {
    let s = secret.trim();
    let bytes = hex::decode(s).map_err(|_| {
        anyhow!("KEY_WRAP_SECRET must be 64 hex characters (openssl rand -hex 32)")
    })?;
    if bytes.len() != 32 {
        bail!("KEY_WRAP_SECRET must be 32 bytes (64 hex characters)");
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&bytes);
    Ok(k)
}

pub fn seal(master: &[u8; 32], plaintext: &str) -> Result<String> {
    let cipher = Aes256Gcm::new(master.into());
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|e| anyhow!("encrypt: {e}"))?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        out,
    ))
}

pub fn open(master: &[u8; 32], blob: &str) -> Result<String> {
    let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, blob)
        .map_err(|e| anyhow!("b64: {e}"))?;
    if raw.len() < 13 {
        return Err(anyhow!("ciphertext too short"));
    }
    let (nonce, ct) = raw.split_at(12);
    let cipher = Aes256Gcm::new(master.into());
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow!("decrypt failed"))?;
    Ok(String::from_utf8(pt).map_err(|e| anyhow!("utf8: {e}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX32: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn wrap_key_requires_32_byte_hex() {
        assert!(wrap_key("dev-secret").is_err());
        assert!(wrap_key("0123456789abcdef").is_err());
        assert!(wrap_key("zzzz").is_err());
        assert!(wrap_key(HEX32).is_ok());
    }

    #[test]
    fn roundtrip() {
        let k = wrap_key(HEX32).unwrap();
        let blob = seal(&k, "poly-secret").unwrap();
        assert_eq!(open(&k, &blob).unwrap(), "poly-secret");
        assert!(open(&k, "not-valid").is_err());
        assert_ne!(blob, seal(&k, "poly-secret").unwrap());
    }
}
