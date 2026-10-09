//! `seal`/`open` from `docs/protocol.md` §8: HMAC-SHA256 in counter mode with
//! an encrypt-then-MAC tag, keyed from a secret two devices saw only in a QR
//! code. Every platform here has HMAC-SHA256 in its standard library.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{Error, Result};

type H = Hmac<Sha256>;

fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut m = H::new_from_slice(key).expect("hmac accepts any key length");
    m.update(msg);
    m.finalize().into_bytes().into()
}

fn keys(secret: &[u8], label: &str) -> ([u8; 32], [u8; 32]) {
    let mut msg = secret.to_vec();
    msg.extend_from_slice(label.as_bytes());
    let k = hmac(b"cued-seal-v1", &msg);
    (hmac(&k, b"enc"), hmac(&k, b"mac"))
}

fn xor_keystream(ke: &[u8; 32], nonce: &[u8; 16], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(32).enumerate() {
        let mut m = [0u8; 20];
        m[..16].copy_from_slice(nonce);
        m[16..].copy_from_slice(&(i as u32).to_be_bytes());
        let ks = hmac(ke, &m);
        for (b, k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= k;
        }
    }
}

/// Seals `plaintext` under `secret` (raw bytes, e.g. the decoded pairing secret) and `label`.
pub fn seal(secret: &[u8], label: &str, plaintext: &[u8]) -> String {
    let (ke, km) = keys(secret, label);
    let nonce: [u8; 16] = rand::random();
    let mut ct = plaintext.to_vec();
    xor_keystream(&ke, &nonce, &mut ct);
    let mut tagged = nonce.to_vec();
    tagged.extend_from_slice(&ct);
    let tag = hmac(&km, &tagged);
    tagged.extend_from_slice(&tag);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tagged)
}

/// Opens what [`seal`] produced; fails on a wrong secret, label or any tampering.
pub fn open(secret: &[u8], label: &str, sealed: &str) -> Result<Vec<u8>> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sealed.trim()).map_err(|_| Error::Seal("not base64url".into()))?;
    if raw.len() < 48 {
        return Err(Error::Seal("too short".into()));
    }
    let (ke, km) = keys(secret, label);
    let (body, tag) = raw.split_at(raw.len() - 32);
    let expected = hmac(&km, body);
    if expected.iter().zip(tag.iter()).fold(0u8, |a, (x, y)| a | (x ^ y)) != 0 {
        return Err(Error::Seal("bad tag: wrong secret or tampered".into()));
    }
    let nonce: [u8; 16] = body[..16].try_into().expect("16 bytes");
    let mut pt = body[16..].to_vec();
    xor_keystream(&ke, &nonce, &mut pt);
    Ok(pt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tamper() {
        let secret = hex::decode("0f".repeat(16)).unwrap();
        let msg = b"{\"secret_hex\":\"...\"}".repeat(7);
        let s = seal(&secret, "account", &msg);
        assert_eq!(open(&secret, "account", &s).unwrap(), msg);
        assert!(open(&secret, "other", &s).is_err());
        assert!(open(&hex::decode("00".repeat(16)).unwrap(), "account", &s).is_err());
        let mut raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&s).unwrap();
        raw[20] ^= 1;
        assert!(open(&secret, "account", &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)).is_err());
        assert!(open(&secret, "account", "zz").is_err());
    }

    /// Fixed vector shared with the Python and Kotlin implementations.
    #[test]
    fn known_vector() {
        let secret = hex::decode("0f".repeat(16)).unwrap();
        let (ke, km) = keys(&secret, "account");
        assert_eq!(hex::encode(ke), "5ea07cd5ce6b821640f080ee725490f4958820c70956cbbaf2bbb850c3b2956d");
        let nonce = [0u8; 16];
        let mut data = b"hello".to_vec();
        xor_keystream(&ke, &nonce, &mut data);
        assert_eq!(hex::encode(&data), "e19510564f");
        let mut body = nonce.to_vec();
        body.extend_from_slice(&data);
        assert_eq!(hex::encode(hmac(&km, &body)), "30e13d3793e94f365e8cb8b3e9fee951adac2972f37a17413bc1e8ea0e1b216b");
        let mut sealed = body;
        sealed.extend_from_slice(&hmac(&km, &sealed.clone()));
        assert_eq!(open(&secret, "account", &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sealed)).unwrap(), b"hello");
    }
}
