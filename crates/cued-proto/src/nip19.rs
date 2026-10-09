//! NIP-19 `npub…` / `nsec…` spellings of 32-byte keys (bech32, BIP-173).

use bech32::{Bech32, Hrp};

pub fn npub(pubkey_hex: &str) -> String {
    encode("npub", pubkey_hex)
}

pub fn nsec(secret_hex: &str) -> String {
    encode("nsec", secret_hex)
}

fn encode(hrp: &str, hex_key: &str) -> String {
    let bytes = hex::decode(hex_key).expect("hex key");
    bech32::encode::<Bech32>(Hrp::parse(hrp).expect("hrp"), &bytes).expect("bech32")
}

/// `("npub", hex)`, `("nsec", hex)` or `("hex", hex)` for a bare 64-char key; None otherwise.
pub fn parse(text: &str) -> Option<(&'static str, String)> {
    let t = text.trim();
    if crate::link::is_hex(t, 64) {
        return Some(("hex", t.to_ascii_lowercase()));
    }
    let (hrp, data) = bech32::decode(t).ok()?;
    if data.len() != 32 {
        return None;
    }
    let kind = match hrp.as_str() {
        "npub" => "npub",
        "nsec" => "nsec",
        _ => return None,
    };
    Some((kind, hex::encode(data)))
}
