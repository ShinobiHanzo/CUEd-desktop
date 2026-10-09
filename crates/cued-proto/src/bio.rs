//! Biometric assertions (`docs/protocol.md` §7): the phone signs a challenge
//! with an ECDSA P-256 key the Android Keystore only uses after a biometric
//! unlock; the desktop verifies with the public key registered at pairing.

use p256::ecdsa::signature::hazmat::PrehashVerifier;
use p256::ecdsa::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// What the phone signs for `device` and `challenge`.
pub fn message(device: &str, challenge: &str) -> Vec<u8> {
    format!("cued-bio|{device}|{challenge}").into_bytes()
}

/// `true` when `sig_hex` (64-byte `r||s` or DER) signs `message(device, challenge)` under `biokey_hex` (SEC1, 33 or 65 bytes).
pub fn verify(biokey_hex: &str, device: &str, challenge: &str, sig_hex: &str) -> bool {
    let (Ok(pk), Ok(sig)) = (hex::decode(biokey_hex.trim()), hex::decode(sig_hex.trim())) else { return false };
    let Ok(vk) = VerifyingKey::from_sec1_bytes(&pk) else { return false };
    let signature = match sig.len() {
        64 => Signature::from_slice(&sig).ok(),
        _ => Signature::from_der(&sig).ok(),
    };
    let Some(signature) = signature else { return false };
    let digest = Sha256::digest(message(device, challenge));
    vk.verify_prehash(&digest, &signature).is_ok()
}

/// `true` when the key is a well-formed P-256 public key.
pub fn valid_biokey(hex_key: &str) -> bool {
    hex::decode(hex_key.trim()).ok().map(|b| VerifyingKey::from_sec1_bytes(&b).is_ok()).unwrap_or(false)
}

pub fn new_challenge() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use p256::ecdsa::SigningKey;

    #[test]
    fn sign_and_verify_both_encodings() {
        let sk = SigningKey::random(&mut rand::thread_rng());
        let pk = hex::encode(sk.verifying_key().to_encoded_point(false).as_bytes());
        assert!(valid_biokey(&pk));
        let ch = new_challenge();
        let digest = Sha256::digest(message("0011223344556677", &ch));
        let sig: Signature = sk.sign_prehash(&digest).unwrap();
        assert!(verify(&pk, "0011223344556677", &ch, &hex::encode(sig.to_bytes())));
        assert!(verify(&pk, "0011223344556677", &ch, &hex::encode(sig.to_der().as_bytes())));
        assert!(!verify(&pk, "0011223344556678", &ch, &hex::encode(sig.to_bytes())));
        assert!(!verify(&pk, "0011223344556677", "aa", &hex::encode(sig.to_bytes())));
        assert!(!valid_biokey("abcd"));
    }
}
