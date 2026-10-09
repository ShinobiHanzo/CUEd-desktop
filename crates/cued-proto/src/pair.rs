//! `cued://pair?…`: linking a phone to a desktop. See `docs/protocol.md` §2.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::link::{build_query, get, is_hex, parse_query};
use crate::{Error, Result, PAIR_VERSION, SCHEME};

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct PairLink {
    pub version: u32,
    /// Desktop name as shown on the phone.
    pub name: String,
    /// Every LAN base URL cuedd listens on, e.g. `http://192.168.1.10:8770`.
    pub addresses: Vec<String>,
    /// Funnel base URL reachable from the internet, if any.
    pub funnel: Option<String>,
    /// SHA-256 fingerprint (hex) of the self-signed TLS cert behind `funnel`.
    pub cert_sha256: Option<String>,
    /// One-time pairing secret, 16 bytes hex.
    pub secret: String,
    /// Desktop's station pubkey (Nostr identity), hex.
    pub pubkey: Option<String>,
    /// The desktop's embedded relay URL.
    pub relay: Option<String>,
}

impl PairLink {
    fn prefix() -> String {
        format!("{SCHEME}://pair?")
    }

    pub fn encode(&self) -> String {
        let mut q: Vec<(&str, String)> = vec![("v", self.version.to_string()), ("n", self.name.clone()), ("a", self.addresses.join(","))];
        if let Some(f) = &self.funnel {
            q.push(("f", f.clone()));
        }
        if let Some(c) = &self.cert_sha256 {
            q.push(("c", c.clone()));
        }
        q.push(("s", self.secret.clone()));
        if let Some(k) = &self.pubkey {
            q.push(("k", k.clone()));
        }
        if let Some(r) = &self.relay {
            q.push(("r", r.clone()));
        }
        format!("{}{}", Self::prefix(), build_query(&q))
    }

    pub fn decode(text: &str) -> Result<Self> {
        let t = text.trim();
        let rest = t.strip_prefix(Self::prefix().as_str()).ok_or(Error::NotALink("pair"))?;
        let q = parse_query(rest);
        let secret = get(&q, "s").ok_or(Error::Missing("s"))?.to_ascii_lowercase();
        if !is_hex(&secret, 32) {
            return Err(Error::Bad("s"));
        }
        let pubkey = get(&q, "k").map(|k| k.to_ascii_lowercase());
        if let Some(k) = &pubkey {
            if !is_hex(k, 64) {
                return Err(Error::Bad("k"));
            }
        }
        Ok(Self {
            version: get(&q, "v").and_then(|v| v.parse().ok()).unwrap_or(PAIR_VERSION),
            name: get(&q, "n").unwrap_or("").to_string(),
            addresses: get(&q, "a").map(|a| a.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default(),
            funnel: get(&q, "f").map(str::to_string),
            cert_sha256: get(&q, "c").map(|c| c.to_ascii_lowercase()),
            secret,
            pubkey,
            relay: get(&q, "r").map(str::to_string),
        })
    }
}

/// Body of `POST /api/pair`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PairRequest {
    #[serde(default = "crate::pair::default_version")]
    pub v: u32,
    pub name: String,
    pub device: String,
    #[serde(default)]
    pub pubkey: String,
    pub nonce: String,
    pub proof: String,
}

pub(crate) fn default_version() -> u32 {
    PAIR_VERSION
}

/// `200` body of `POST /api/pair`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PairResponse {
    pub token: String,
    pub desktop: DesktopInfo,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DesktopInfo {
    pub name: String,
    pub pubkey: String,
    pub relay: String,
    pub funnel: Option<String>,
    pub cert: Option<String>,
}

/// `HMAC-SHA256(secret bytes, device|name|nonce)` as hex. Both sides compute this.
pub fn proof(secret_hex: &str, device: &str, name: &str, nonce: &str) -> Result<String> {
    let key = hex::decode(secret_hex)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| Error::Bad("s"))?;
    mac.update(format!("{device}|{name}|{nonce}").as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

impl PairRequest {
    /// What a phone builds after scanning `link`.
    pub fn build(link: &PairLink, name: &str, device: &str, pubkey: &str) -> Result<Self> {
        let nonce = hex::encode(rand::random::<[u8; 8]>());
        let proof = proof(&link.secret, device, name, &nonce)?;
        Ok(Self { v: PAIR_VERSION, name: name.into(), device: device.into(), pubkey: pubkey.into(), nonce, proof })
    }

    /// Constant-time check against the secret the desktop issued.
    pub fn verify(&self, secret_hex: &str) -> bool {
        let Ok(expected) = proof(secret_hex, &self.device, &self.name, &self.nonce) else { return false };
        let (Ok(a), Ok(b)) = (hex::decode(&expected), hex::decode(&self.proof)) else { return false };
        a.len() == b.len() && a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

pub fn new_secret() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_round_trip() {
        let l = PairLink {
            version: 1,
            name: "Idris's laptop".into(),
            addresses: vec!["http://192.168.1.10:8770".into(), "http://10.8.0.2:8770".into()],
            funnel: Some("https://home.example.net:443".into()),
            cert_sha256: Some("ab".repeat(32)),
            secret: new_secret(),
            pubkey: Some("3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d".into()),
            relay: Some("ws://192.168.1.10:8770/relay".into()),
        };
        let s = l.encode();
        assert!(s.starts_with("cued://pair?v=1&n=Idris%27s%20laptop&a=http%3A%2F%2F192.168.1.10%3A8770%2Chttp%3A%2F%2F10.8.0.2%3A8770&f="));
        assert_eq!(PairLink::decode(&s).unwrap(), l);
        assert!(PairLink::decode("cued://pair?n=x").is_err());
        assert!(PairLink::decode("cued://pair?s=nothex").is_err());
    }

    #[test]
    fn proof_verifies_and_rejects() {
        let l = PairLink { secret: new_secret(), ..Default::default() };
        let req = PairRequest::build(&l, "Pixel 8", "0011223344556677", "").unwrap();
        assert!(req.verify(&l.secret));
        assert!(!req.verify(&new_secret()));
        let mut tampered = req.clone();
        tampered.name = "Other".into();
        assert!(!tampered.verify(&l.secret));
        assert_eq!(proof("00".repeat(16).as_str(), "d", "n", "x").unwrap().len(), 64);
        // Shared vector with the phone's PairLinkTest and the Python tests.
        assert_eq!(proof(&"0f".repeat(16), "0011223344556677", "Pixel 8", "deadbeef").unwrap(), "78f4743fd76a4f52d564add687136ba32ea146005f309f237bee9781d13048d7");
    }
}
