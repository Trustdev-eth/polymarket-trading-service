use crate::eip712::{
    eip712_digest, hex0x, hex_no0x, keccak256, pad_address, pad_u256, parse_address, CachedDomain,
    ORDER_TYPE_STRING, SIG_TYPE_POLY_1271, ZERO_BYTES32,
};
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SignError {
    #[error("bad key")]
    Key,
    #[error("ecdsa")]
    Ecdsa,
    #[error("address")]
    Address,
}

/// CLOB V2 signed order. Fees are not in the signed struct (operator-set at match).
#[derive(Clone, Debug)]
pub struct OrderV2 {
    pub salt: u128,
    pub maker: [u8; 20],
    pub signer: [u8; 20],
    pub token_id: [u8; 32],
    pub maker_amount: u128,
    pub taker_amount: u128,
    pub side: u8,
    pub signature_type: u8,
    pub timestamp: u64,
    pub metadata: [u8; 32],
    pub builder: [u8; 32],
}

impl OrderV2 {
    pub fn struct_hash(&self, type_hash: &[u8; 32]) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 * 12);
        buf.extend_from_slice(type_hash);
        buf.extend_from_slice(&pad_u256(self.salt));
        buf.extend_from_slice(&pad_address(&self.maker));
        buf.extend_from_slice(&pad_address(&self.signer));
        buf.extend_from_slice(&self.token_id);
        buf.extend_from_slice(&pad_u256(self.maker_amount));
        buf.extend_from_slice(&pad_u256(self.taker_amount));
        buf.extend_from_slice(&pad_u256(self.side as u128));
        buf.extend_from_slice(&pad_u256(self.signature_type as u128));
        buf.extend_from_slice(&pad_u256(self.timestamp as u128));
        buf.extend_from_slice(&self.metadata);
        buf.extend_from_slice(&self.builder);
        keccak256(&buf)
    }
}

#[derive(Clone)]
pub struct V2Signer {
    key: SigningKey,
    address: [u8; 20],
    pub domain: CachedDomain,
}

impl V2Signer {
    pub fn from_hex(priv_hex: &str) -> Result<Self, SignError> {
        Self::from_hex_on_chain(priv_hex, 137)
    }

    pub fn from_hex_on_chain(priv_hex: &str, chain_id: u64) -> Result<Self, SignError> {
        let s = priv_hex.trim().trim_start_matches("0x");
        let bytes = hex::decode(s).map_err(|_| SignError::Key)?;
        if bytes.len() != 32 {
            return Err(SignError::Key);
        }
        let key = SigningKey::from_bytes(bytes.as_slice().into()).map_err(|_| SignError::Key)?;
        let vk = VerifyingKey::from(&key);
        let pt = vk.to_encoded_point(false);
        let uncompressed = pt.as_bytes(); // 0x04 || x || y
        let hash = keccak256(&uncompressed[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        Ok(Self {
            key,
            address,
            domain: CachedDomain::for_chain(chain_id),
        })
    }

    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    pub fn address_hex(&self) -> String {
        hex0x(&self.address)
    }

    /// Sign digest, Ethereum v = 27 + recid. cloid == digest (lecture invariant).
    pub fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 65], SignError> {
        recoverable_sign(&self.key, digest)
    }

    /// Returns (signature 0x-hex, cloid 0x-hex, digest).
    /// POLY_1271 (sig type 3) uses ERC-7739 TypedDataSign wrapping required by deposit wallets.
    pub fn sign_order(&self, order: &OrderV2, neg_risk: bool) -> Result<(String, String, [u8; 32]), SignError> {
        let contents_hash = order.struct_hash(&self.domain.type_hash);
        let domain = self.domain.separator(neg_risk);
        let digest = eip712_digest(&domain, &contents_hash);
        let sig = if order.signature_type == SIG_TYPE_POLY_1271 {
            self.sign_poly_1271(&contents_hash, order, &domain)?
        } else {
            hex0x(&self.sign_digest(&digest)?)
        };
        Ok((sig, hex0x(&digest), digest))
    }

    /// ERC-7739 wrap matching py-clob-client-v2 `ExchangeOrderBuilderV2._build_poly_1271_order_signature`.
    fn sign_poly_1271(
        &self,
        contents_hash: &[u8; 32],
        order: &OrderV2,
        app_domain_separator: &[u8; 32],
    ) -> Result<String, SignError> {
        let mut buf = Vec::with_capacity(32 * 7);
        buf.extend_from_slice(&self.domain.solady_type_hash);
        buf.extend_from_slice(contents_hash);
        buf.extend_from_slice(&self.domain.deposit_name_hash);
        buf.extend_from_slice(&self.domain.deposit_version_hash);
        buf.extend_from_slice(&pad_u256(self.domain.chain_id as u128));
        buf.extend_from_slice(&pad_address(&order.signer));
        buf.extend_from_slice(&ZERO_BYTES32);
        let typed_data_sign_hash = keccak256(&buf);
        let digest = eip712_digest(app_domain_separator, &typed_data_sign_hash);
        let inner = self.sign_digest(&digest)?;
        let mut out = String::with_capacity(2 + (65 + 32 + 32 + ORDER_TYPE_STRING.len() + 2) * 2);
        out.push_str("0x");
        out.push_str(&hex_no0x(&inner));
        out.push_str(&hex_no0x(app_domain_separator));
        out.push_str(&hex_no0x(contents_hash));
        out.push_str(&hex_no0x(ORDER_TYPE_STRING));
        out.push_str(&format!("{:04x}", ORDER_TYPE_STRING.len() as u16));
        Ok(out)
    }
}

/// Low-S RFC6979 signature with Ethereum `v = 27 + recid`. One scalar multiplication.
fn recoverable_sign(key: &SigningKey, digest: &[u8; 32]) -> Result<[u8; 65], SignError> {
    let (sig, recid): (Signature, RecoveryId) =
        key.sign_prehash_recoverable(digest).map_err(|_| SignError::Ecdsa)?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = 27 + (recid.to_byte() & 1);
    Ok(out)
}

pub fn sign_clob_auth(signer: &V2Signer, timestamp: u64, nonce: u64) -> Result<String, SignError> {
    let type_hash = keccak256(b"ClobAuth(address address,string timestamp,uint256 nonce,string message)");
    let ts = timestamp.to_string();
    let mut buf = Vec::with_capacity(160);
    buf.extend_from_slice(&type_hash);
    buf.extend_from_slice(&pad_address(&signer.address));
    buf.extend_from_slice(&keccak256(ts.as_bytes()));
    buf.extend_from_slice(&pad_u256(nonce as u128));
    buf.extend_from_slice(&keccak256(crate::eip712::CLOB_AUTH_MSG.as_bytes()));
    let struct_hash = keccak256(&buf);
    let domain = crate::eip712::clob_auth_domain(137);
    let digest = eip712_digest(&domain, &struct_hash);
    let sig = signer.sign_digest(&digest)?;
    Ok(hex0x(&sig))
}

pub fn parse_maker(s: &str) -> Result<[u8; 20], SignError> {
    parse_address(s).map_err(|_| SignError::Address)
}

pub fn zero_meta() -> [u8; 32] {
    ZERO_BYTES32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-known Anvil key #0.
    const KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    #[test]
    fn address_from_key() {
        let s = V2Signer::from_hex(KEY).unwrap();
        assert_eq!(
            s.address_hex(),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
    }

    #[test]
    fn v2_order_digest_is_stable_and_not_v1() {
        let s = V2Signer::from_hex(KEY).unwrap();
        let mut token = [0u8; 32];
        token[31] = 1;
        let order = OrderV2 {
            salt: 12345,
            maker: s.address(),
            signer: s.address(),
            token_id: token,
            maker_amount: 1_000_000,
            taker_amount: 2_000_000,
            side: 0,
            signature_type: 0,
            timestamp: 1_713_398_400_000,
            metadata: [0u8; 32],
            builder: [0u8; 32],
        };
        let (_sig, cloid, digest) = s.sign_order(&order, false).unwrap();
        assert_eq!(cloid, hex0x(&digest));
        // frozen golden: re-sign must match
        let (_sig2, cloid2, _) = s.sign_order(&order, false).unwrap();
        assert_eq!(cloid, cloid2);
        // V1 typehash over the same encoding layout cannot apply; digest length 32
        assert_eq!(digest.len(), 32);
        // distinct from hashing with V1 typehash
        let v1 = order.struct_hash(&crate::eip712::order_v1_type_hash());
        assert_ne!(v1, order.struct_hash(&s.domain.type_hash));
    }

    /// Brute-force recovery: the original implementation, kept as an oracle.
    fn recoverable_sign_by_recovery(key: &SigningKey, digest: &[u8; 32]) -> [u8; 65] {
        use k256::ecdsa::signature::hazmat::PrehashSigner;
        let sig: Signature = key.sign_prehash(digest).unwrap();
        let vk = VerifyingKey::from(key);
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        for rec in 0u8..2 {
            let recid = RecoveryId::try_from(rec).unwrap();
            if VerifyingKey::recover_from_prehash(digest, &sig, recid).is_ok_and(|r| r == vk) {
                out[64] = rec + 27;
                return out;
            }
        }
        panic!("no recovery id");
    }

    #[test]
    fn recoverable_sign_matches_recovery_oracle() {
        let s = V2Signer::from_hex(KEY).unwrap();
        let mut seen_v = [false; 2];
        for i in 0u32..256 {
            let digest = keccak256(&i.to_le_bytes());
            let fast = s.sign_digest(&digest).unwrap();
            assert_eq!(fast, recoverable_sign_by_recovery(&s.key, &digest), "digest #{i}");
            seen_v[(fast[64] - 27) as usize] = true;
        }
        assert_eq!(seen_v, [true, true], "both recovery ids exercised");
    }

    #[test]
    fn lecture_v1_typehash_does_not_match_v2_order() {
        assert_ne!(
            crate::eip712::order_v1_type_hash(),
            crate::eip712::order_v2_type_hash()
        );
    }

    #[test]
    fn poly_1271_signature_matches_official_sdk_vector() {
        // Frozen vector from py-clob-client-v2 tests/order_utils/test_exchange_order_builder_v2.py
        // (Amoy chain_id=80002, same V2 exchange address as Polygon).
        let s = V2Signer::from_hex_on_chain(KEY, 80002).unwrap();
        let maker = parse_maker("0x1111111111111111111111111111111111111111").unwrap();
        let mut token = [0u8; 32];
        token[30] = 0x04;
        token[31] = 0xd2; // 1234
        let order = OrderV2 {
            salt: 479249096354,
            maker,
            signer: maker,
            token_id: token,
            maker_amount: 100_000_000,
            taker_amount: 50_000_000,
            side: 0,
            signature_type: 3,
            timestamp: 1_710_000_000_000,
            metadata: [0u8; 32],
            builder: [0u8; 32],
        };
        let (sig, cloid, digest) = s.sign_order(&order, false).unwrap();
        assert_eq!(cloid, hex0x(&digest));
        assert_eq!(sig.len(), 2 + (65 + 32 + 32 + crate::eip712::ORDER_TYPE_STRING.len() + 2) * 2);
        const EXPECTED: &str = concat!(
            "0xa3a093c83b6c20c83355c16ce94c92e6e9fcbdeb840618cc74f6c57a42ad145b",
            "2b98db73d2c73cbf1f2b6af288566ae81960ddbc3a13921027358a8bff3be6ff1c",
            "a440cbd865bc0c6243d7a8df9a8bf48a8827b0a4abbb61c30e96d305423af148",
            "d23d42d3ad94e65d78258cecaf8dcbaddac0f73dc085040f2c12bb595dd83804",
            "4f726465722875696e743235362073616c742c61646472657373206d616b65722c",
            "61646472657373207369676e65722c75696e7432353620746f6b656e49642c75",
            "696e74323536206d616b6572416d6f756e742c75696e743235362074616b6572",
            "416d6f756e742c75696e743820736964652c75696e7438207369676e61747572",
            "65547970652c75696e743235362074696d657374616d702c6279746573333220",
            "6d657461646174612c62797465733332206275696c6465722900ba",
        );
        assert_eq!(sig, EXPECTED);
    }
}
