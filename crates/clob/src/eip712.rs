use tiny_keccak::{Hasher, Keccak};

#[inline]
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut k = Keccak::v256();
    k.update(data);
    k.finalize(&mut out);
    out
}

#[inline]
pub fn pad_u256(x: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&x.to_be_bytes());
    out
}

pub fn pad_u256_be(bytes32: &[u8; 32]) -> [u8; 32] {
    *bytes32
}

pub fn u256_from_dec(s: &str) -> Result<[u8; 32], &'static str> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty u256");
    }
    let mut acc = [0u8; 32];
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return Err("non-digit in u256");
        }
        let d = b - b'0';
        let mut carry = d as u16;
        for i in (0..32).rev() {
            let v = acc[i] as u16 * 10 + carry;
            acc[i] = (v & 0xff) as u8;
            carry = v >> 8;
        }
        if carry != 0 {
            return Err("u256 overflow");
        }
    }
    Ok(acc)
}

pub fn pad_address(addr: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr);
    out
}

pub fn parse_address(s: &str) -> Result<[u8; 20], &'static str> {
    let s = s.trim().trim_start_matches("0x");
    if s.len() != 40 {
        return Err("address len");
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "hex")?;
    }
    Ok(out)
}

pub fn parse_bytes32(s: &str) -> Result<[u8; 32], &'static str> {
    let s = s.trim().trim_start_matches("0x");
    if s.len() != 64 {
        return Err("bytes32 len");
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "hex")?;
    }
    Ok(out)
}

pub fn u256_to_dec(b: &[u8; 32]) -> String {
    let mut digits = vec![0u8];
    for &byte in b {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            let v = (*d as u32) * 256 + carry;
            *d = (v % 10) as u8;
            carry = v / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    while digits.len() > 1 && *digits.last().unwrap() == 0 {
        digits.pop();
    }
    digits
        .iter()
        .rev()
        .map(|d| char::from(b'0' + *d))
        .collect()
}

pub fn hex0x(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    s.push_str(&hex::encode(bytes));
    s
}

pub fn hex_no0x(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Official CLOB V2 Order type string (no spaces). Used for EIP-712 and ERC-7739 trailer.
pub const ORDER_TYPE_STRING: &[u8] = b"Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";

pub const SIG_TYPE_EOA: u8 = 0;
pub const SIG_TYPE_POLY_PROXY: u8 = 1;
pub const SIG_TYPE_GNOSIS_SAFE: u8 = 2;
pub const SIG_TYPE_POLY_1271: u8 = 3;

/// EIP-712 domain separator for Exchange V2.
pub fn domain_separator(name: &str, version: &str, chain_id: u64, verifying: &[u8; 20]) -> [u8; 32] {
    let type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut buf = Vec::with_capacity(160);
    buf.extend_from_slice(&type_hash);
    buf.extend_from_slice(&keccak256(name.as_bytes()));
    buf.extend_from_slice(&keccak256(version.as_bytes()));
    let mut cid = [0u8; 32];
    cid[24..].copy_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(&cid);
    buf.extend_from_slice(&pad_address(verifying));
    keccak256(&buf)
}

/// ClobAuthDomain has no verifyingContract (L1 headers, unchanged in V2).
pub fn clob_auth_domain(chain_id: u64) -> [u8; 32] {
    let type_hash = keccak256(b"EIP712Domain(string name,string version,uint256 chainId)");
    let mut buf = Vec::with_capacity(128);
    buf.extend_from_slice(&type_hash);
    buf.extend_from_slice(&keccak256(b"ClobAuthDomain"));
    buf.extend_from_slice(&keccak256(b"1"));
    let mut cid = [0u8; 32];
    cid[24..].copy_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(&cid);
    keccak256(&buf)
}

pub fn eip712_digest(domain: &[u8; 32], struct_hash: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 66];
    buf[0] = 0x19;
    buf[1] = 0x01;
    buf[2..34].copy_from_slice(domain);
    buf[34..].copy_from_slice(struct_hash);
    keccak256(&buf)
}

/// Cached V2 Order TYPE_HASH. Lecture V1 hashes fail on purpose.
pub fn order_v2_type_hash() -> [u8; 32] {
    keccak256(ORDER_TYPE_STRING)
}

/// Solady ERC-7739 `TypedDataSign` typehash used by deposit wallets (POLY_1271).
pub fn solady_typed_data_sign_type_hash() -> [u8; 32] {
    let mut s = Vec::with_capacity(128 + ORDER_TYPE_STRING.len());
    s.extend_from_slice(
        b"TypedDataSign(Order contents,string name,string version,uint256 chainId,address verifyingContract,bytes32 salt)",
    );
    s.extend_from_slice(ORDER_TYPE_STRING);
    keccak256(&s)
}

pub fn order_v1_type_hash() -> [u8; 32] {
    keccak256(
        b"Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,uint256 feeRateBps,uint8 side,uint8 signatureType)",
    )
}

pub const EXCHANGE_NAME: &str = "Polymarket CTF Exchange";
pub const EXCHANGE_VERSION: &str = "2";
pub const EXCHANGE_V2: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
pub const NEG_RISK_EXCHANGE_V2: &str = "0xe2222d279d744050d28e00520010520000310F59";
pub const ZERO_BYTES32: [u8; 32] = [0u8; 32];
pub const CLOB_AUTH_MSG: &str = "This message attests that I control the given wallet";

#[derive(Clone, Debug)]
pub struct CachedDomain {
    pub chain_id: u64,
    pub exchange: [u8; 32],
    pub neg_risk: [u8; 32],
    pub type_hash: [u8; 32],
    pub solady_type_hash: [u8; 32],
    pub deposit_name_hash: [u8; 32],
    pub deposit_version_hash: [u8; 32],
    pub exchange_addr: [u8; 20],
    pub neg_risk_addr: [u8; 20],
}

impl CachedDomain {
    pub fn polygon_v2() -> Self {
        Self::for_chain(137)
    }

    pub fn for_chain(chain_id: u64) -> Self {
        let exchange_addr = parse_address(EXCHANGE_V2).expect("exchange");
        let neg_risk_addr = parse_address(NEG_RISK_EXCHANGE_V2).expect("neg-risk");
        Self {
            chain_id,
            exchange: domain_separator(EXCHANGE_NAME, EXCHANGE_VERSION, chain_id, &exchange_addr),
            neg_risk: domain_separator(EXCHANGE_NAME, EXCHANGE_VERSION, chain_id, &neg_risk_addr),
            type_hash: order_v2_type_hash(),
            solady_type_hash: solady_typed_data_sign_type_hash(),
            deposit_name_hash: keccak256(b"DepositWallet"),
            deposit_version_hash: keccak256(b"1"),
            exchange_addr,
            neg_risk_addr,
        }
    }

    pub fn separator(&self, neg_risk: bool) -> [u8; 32] {
        if neg_risk {
            self.neg_risk
        } else {
            self.exchange
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_type_hash_differs_from_v1() {
        assert_ne!(order_v2_type_hash(), order_v1_type_hash());
        // freeze keccak of the official V2 type string
        let frozen = order_v2_type_hash();
        assert_eq!(frozen, keccak256(b"Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)"));
    }

    #[test]
    fn addresses_are_v2() {
        let d = CachedDomain::polygon_v2();
        assert_eq!(hex0x(&d.exchange_addr), EXCHANGE_V2.to_lowercase());
        assert_ne!(
            domain_separator(EXCHANGE_NAME, "1", 137, &d.exchange_addr),
            d.exchange
        );
    }

    #[test]
    fn u256_from_dec_small() {
        let v = u256_from_dec("255").unwrap();
        assert_eq!(v[31], 255);
        assert!(v[..31].iter().all(|&b| b == 0));
        assert_eq!(u256_to_dec(&v), "255");
        let big = u256_from_dec("13823452485754155775626978493272584781932111241929813181639400319370641792304").unwrap();
        assert_eq!(
            u256_to_dec(&big),
            "13823452485754155775626978493272584781932111241929813181639400319370641792304"
        );
    }
}
