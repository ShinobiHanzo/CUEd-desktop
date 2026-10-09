//! The account chain (`docs/protocol.md` §8): a signed, hash-linked list of
//! blocks only the account key can extend. Every device of the account holds
//! a full copy; copies merge by "longer valid chain wins".

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::nostr::{verify_schnorr, Identity};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub v: u32,
    pub height: u64,
    pub prev: String,
    pub ts: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
    pub pubkey: String,
    pub hash: String,
    pub sig: String,
}

impl Block {
    pub fn compute_hash(v: u32, height: u64, prev: &str, ts: i64, kind: &str, data: &Value, pubkey: &str) -> String {
        let canonical = serde_json::to_string(&json!([v, height, prev, ts, kind, data, pubkey])).expect("json");
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }

    /// Hash matches the fields and the signature is the account key's.
    pub fn verify(&self) -> bool {
        self.v == 1 && self.hash == Self::compute_hash(self.v, self.height, &self.prev, self.ts, &self.kind, &self.data, &self.pubkey) && verify_schnorr(&self.hash, &self.pubkey, &self.sig)
    }
}

/// `sha256("cued-device|" + device id + "|" + salt)` hex.
pub fn devhash(device_id: &str, salt: &str) -> String {
    hex::encode(Sha256::digest(format!("cued-device|{device_id}|{salt}").as_bytes()))
}

/// Account id: `sha256(pubkey bytes)` hex.
pub fn account_id(pubkey_hex: &str) -> String {
    hex::encode(Sha256::digest(hex::decode(pubkey_hex).unwrap_or_default()))
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Chain {
    pub blocks: Vec<Block>,
}

impl Chain {
    /// A new account: block 0 is `create {name}`.
    pub fn create(id: &Identity, name: &str, ts: i64) -> Self {
        let mut c = Chain::default();
        c.append(id, "create", json!({ "name": name }), ts).expect("genesis");
        c
    }

    pub fn pubkey(&self) -> Option<&str> {
        self.blocks.first().map(|b| b.pubkey.as_str())
    }
    pub fn height(&self) -> u64 {
        self.blocks.len() as u64
    }
    pub fn tip(&self) -> Option<&Block> {
        self.blocks.last()
    }
    pub fn tip_hash(&self) -> String {
        self.tip().map(|b| b.hash.clone()).unwrap_or_default()
    }

    /// Signs and appends a block. `id` must be the account's key.
    pub fn append(&mut self, id: &Identity, kind: &str, data: Value, ts: i64) -> Result<&Block> {
        let pubkey = id.pubkey_hex();
        if let Some(p) = self.pubkey() {
            if p != pubkey {
                return Err(Error::Chain("only the account key can extend its chain".into()));
            }
        }
        let height = self.height();
        let prev = self.tip_hash();
        let hash = Block::compute_hash(1, height, &prev, ts, kind, &data, &pubkey);
        let sig = hex::encode(id.sign_raw(&hex::decode(&hash).expect("hex"), &rand::random()));
        self.blocks.push(Block { v: 1, height, prev, ts, kind: kind.into(), data, pubkey, hash, sig });
        Ok(self.blocks.last().expect("just pushed"))
    }

    /// Every block well-formed, linked, consecutive and signed by block 0's key.
    pub fn validate(&self) -> Result<()> {
        let Some(first) = self.blocks.first() else { return Err(Error::Chain("empty".into())) };
        if first.kind != "create" || !first.prev.is_empty() || first.height != 0 {
            return Err(Error::Chain("block 0 must be create".into()));
        }
        let mut prev = String::new();
        for (i, b) in self.blocks.iter().enumerate() {
            if b.height != i as u64 || b.prev != prev || b.pubkey != first.pubkey || !b.verify() {
                return Err(Error::Chain(format!("block {i} is not valid")));
            }
            prev = b.hash.clone();
        }
        Ok(())
    }

    /// Merges `other` into self: the longer valid chain wins (equal height: lower tip hash);
    /// returns `true` when self changed. Both must be the same account.
    pub fn merge(&mut self, other: &Chain) -> Result<bool> {
        other.validate()?;
        if self.blocks.is_empty() {
            *self = other.clone();
            return Ok(true);
        }
        if self.pubkey() != other.pubkey() {
            return Err(Error::Chain("different account".into()));
        }
        let take_other = other.height() > self.height() || (other.height() == self.height() && other.tip_hash() < self.tip_hash());
        if take_other && other.tip_hash() != self.tip_hash() {
            *self = other.clone();
            return Ok(true);
        }
        Ok(false)
    }

    /// Latest `data` of blocks of this kind.
    pub fn latest(&self, kind: &str) -> Option<&Value> {
        self.blocks.iter().rev().find(|b| b.kind == kind).map(|b| &b.data)
    }

    /// Display name: the latest `profile`, else `create`.
    pub fn name(&self) -> String {
        self.latest("profile").or_else(|| self.latest("create")).and_then(|d| d["name"].as_str()).unwrap_or("").to_string()
    }

    /// Devices currently bound: `bind_device` minus later `unbind_device`.
    pub fn devices(&self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for b in &self.blocks {
            match b.kind.as_str() {
                "bind_device" => {
                    let dh = b.data["devhash"].as_str().unwrap_or("").to_string();
                    out.retain(|d| d["devhash"].as_str() != Some(dh.as_str()));
                    let mut d = b.data.clone();
                    d["boundAt"] = json!(b.ts);
                    out.push(d);
                }
                "unbind_device" => {
                    let dh = b.data["devhash"].as_str().unwrap_or("");
                    out.retain(|d| d["devhash"].as_str() != Some(dh));
                }
                _ => {}
            }
        }
        out
    }

    /// The shared settings object: every `settings` block merged in order (later keys win).
    pub fn settings(&self) -> Value {
        let mut acc = json!({});
        for b in self.blocks.iter().filter(|b| b.kind == "settings") {
            if let (Some(a), Some(d)) = (acc.as_object_mut(), b.data.as_object()) {
                for (k, v) in d {
                    a.insert(k.clone(), v.clone());
                }
            }
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_validate_merge() {
        let id = Identity::generate();
        let mut a = Chain::create(&id, "Idris", 1);
        a.append(&id, "bind_device", json!({"devhash": devhash("0011223344556677", "salt"), "name": "Pixel", "kind": "phone"}), 2).unwrap();
        a.validate().unwrap();
        assert_eq!(a.name(), "Idris");
        assert_eq!(a.devices().len(), 1);
        let mut b = a.clone();
        b.append(&id, "settings", json!({"relayEnabled": true}), 3).unwrap();
        b.append(&id, "settings", json!({"theme": {"accent": "#fff"}}), 4).unwrap();
        assert_eq!(b.settings(), json!({"relayEnabled": true, "theme": {"accent": "#fff"}}));
        assert!(a.merge(&b).unwrap());
        assert_eq!(a.height(), 4);
        assert!(!a.merge(&b).unwrap());
        // A shorter copy never wins.
        let short = Chain { blocks: a.blocks[..2].to_vec() };
        assert!(!a.merge(&short).unwrap());
        // Tampering breaks validation.
        let mut bad = a.clone();
        bad.blocks[1].data["name"] = json!("Other");
        assert!(bad.validate().is_err());
        // Another key cannot extend it.
        let other = Identity::generate();
        assert!(a.append(&other, "profile", json!({"name": "x"}), 5).is_err());
        let foreign = Chain::create(&other, "x", 1);
        assert!(a.merge(&foreign).is_err());
        // unbind
        a.append(&id, "unbind_device", json!({"devhash": devhash("0011223344556677", "salt")}), 6).unwrap();
        assert!(a.devices().is_empty());
        assert_eq!(account_id(&id.pubkey_hex()).len(), 64);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Chain>(&json).unwrap(), a);
    }
}
