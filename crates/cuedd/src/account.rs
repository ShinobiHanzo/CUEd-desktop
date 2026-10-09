//! The desktop's account: its key pair, the account chain and the shared
//! settings that live in it. `docs/protocol.md` §8 and §11.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use cued_proto::account::{account_id, devhash, Chain};
use cued_proto::Identity;
use serde_json::{json, Value};

/// What every device of the account shares, with the defaults a fresh account starts from.
pub fn default_shared() -> Value {
    json!({
        "relayEnabled": false,
        "friendStreaming": true,
        "theme": {
            "bg": "#0e0f13", "panel": "#161820", "panel2": "#1d2029", "text": "#ececf1", "muted": "#8d92a3",
            "accent": "#5ef2c6", "accent2": "#7aa2ff", "danger": "#ff6b7a", "border": "#262a36"
        },
        "stationName": ""
    })
}

fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => deep_merge(existing, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

pub struct AccountStore {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    identity: Identity,
    chain: Chain,
    /// Per-install random salt behind this desktop's device hash.
    salt: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Meta {
    salt: String,
}

impl AccountStore {
    /// Loads `identity.json` (the key), `account.json` (the chain) and `device.json` (the salt), creating what is missing.
    pub fn open(dir: &Path, name: &str) -> anyhow::Result<Self> {
        let identity = crate::identity::load_or_create(&dir.join("identity.json"))?;
        let meta_path = dir.join("device.json");
        let meta: Meta = match std::fs::read_to_string(&meta_path) {
            Ok(t) => serde_json::from_str(&t)?,
            Err(_) => {
                let m = Meta { salt: hex::encode(rand::random::<[u8; 16]>()) };
                std::fs::write(&meta_path, serde_json::to_vec_pretty(&m)?)?;
                m
            }
        };
        let chain_path = dir.join("account.json");
        let mut chain: Chain = std::fs::read_to_string(&chain_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let now = cued_proto::nostr::now_secs();
        if chain.blocks.is_empty() || chain.pubkey() != Some(identity.pubkey_hex().as_str()) || chain.validate().is_err() {
            chain = Chain::create(&identity, name, now);
        }
        let me = devhash(&identity.pubkey_hex(), &meta.salt);
        if !chain.devices().iter().any(|d| d["devhash"].as_str() == Some(me.as_str())) {
            chain.append(&identity, "bind_device", json!({"devhash": me, "name": name, "kind": "desktop"}), now).context("binding this desktop")?;
        }
        let s = Self { dir: dir.to_path_buf(), inner: Mutex::new(Inner { identity, chain, salt: meta.salt }) };
        s.save()?;
        Ok(s)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self) -> anyhow::Result<()> {
        let i = self.lock();
        let tmp = self.dir.join("account.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&i.chain)?)?;
        std::fs::rename(tmp, self.dir.join("account.json"))?;
        Ok(())
    }

    pub fn identity(&self) -> Identity {
        self.lock().identity.clone()
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pubkey_hex(&self) -> String {
        self.lock().identity.pubkey_hex()
    }
    pub fn chain(&self) -> Chain {
        self.lock().chain.clone()
    }
    /// This desktop's device hash.
    pub fn my_devhash(&self) -> String {
        let i = self.lock();
        devhash(&i.identity.pubkey_hex(), &i.salt)
    }

    pub fn summary(&self) -> Value {
        let i = self.lock();
        let pk = i.identity.pubkey_hex();
        json!({
            "id": account_id(&pk),
            "npub": i.identity.npub(),
            "pubkey": pk,
            "name": i.chain.name(),
            "height": i.chain.height(),
            "tip": i.chain.tip_hash(),
            "thisDevice": devhash(&pk, &i.salt),
            "devices": i.chain.devices(),
        })
    }

    /// Shared settings: defaults overlaid with every `settings` block.
    pub fn shared(&self) -> Value {
        let i = self.lock();
        let mut v = default_shared();
        deep_merge(&mut v, &i.chain.settings());
        v["stationName"] = v["stationName"].clone();
        v["updatedAt"] = json!(i.chain.blocks.iter().rev().find(|b| b.kind == "settings").map(|b| b.ts).unwrap_or(0));
        v["height"] = json!(i.chain.height());
        v
    }

    pub fn relay_enabled(&self) -> bool {
        self.shared()["relayEnabled"].as_bool().unwrap_or(false)
    }
    pub fn friend_streaming(&self) -> bool {
        self.shared()["friendStreaming"].as_bool().unwrap_or(true)
    }

    /// Appends a `settings` block with `patch` (only the keys that are allowed), returns the merged settings.
    pub fn update_shared(&self, patch: &Value) -> anyhow::Result<Value> {
        let mut clean = json!({});
        if let Some(o) = patch.as_object() {
            for (k, v) in o {
                match k.as_str() {
                    "relayEnabled" | "friendStreaming" => {
                        if v.is_boolean() {
                            clean[k] = v.clone();
                        }
                    }
                    "theme" => {
                        if let Some(t) = v.as_object() {
                            let mut theme = json!({});
                            for (tk, tv) in t {
                                if let Some(s) = tv.as_str() {
                                    if s.len() <= 9 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit()) && tk.len() <= 16 {
                                        theme[tk] = json!(s);
                                    }
                                }
                            }
                            clean["theme"] = theme;
                        }
                    }
                    "stationName" => {
                        if let Some(s) = v.as_str() {
                            clean[k] = json!(s.trim().chars().take(40).collect::<String>());
                        }
                    }
                    _ => {}
                }
            }
        }
        if clean.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            anyhow::bail!("nothing to change");
        }
        {
            let mut i = self.lock();
            let id = i.identity.clone();
            i.chain.append(&id, "settings", clean, cued_proto::nostr::now_secs())?;
        }
        self.save()?;
        Ok(self.shared())
    }

    pub fn set_name(&self, name: &str) -> anyhow::Result<()> {
        {
            let mut i = self.lock();
            let id = i.identity.clone();
            i.chain.append(&id, "profile", json!({"name": name.trim().chars().take(40).collect::<String>()}), cued_proto::nostr::now_secs())?;
        }
        self.save()
    }

    pub fn bind_device(&self, devhash: &str, name: &str, kind: &str) -> anyhow::Result<()> {
        {
            let mut i = self.lock();
            if i.chain.devices().iter().any(|d| d["devhash"].as_str() == Some(devhash)) {
                return Ok(());
            }
            let id = i.identity.clone();
            i.chain.append(&id, "bind_device", json!({"devhash": devhash, "name": name, "kind": kind}), cued_proto::nostr::now_secs())?;
        }
        self.save()
    }

    pub fn unbind_device(&self, devhash: &str) -> anyhow::Result<()> {
        if devhash == self.my_devhash() {
            anyhow::bail!("cannot unbind this desktop from itself");
        }
        {
            let mut i = self.lock();
            let id = i.identity.clone();
            i.chain.append(&id, "unbind_device", json!({"devhash": devhash}), cued_proto::nostr::now_secs())?;
        }
        self.save()
    }

    /// Merges a chain copy from another device of the same account. Returns true when ours changed.
    pub fn merge(&self, other: &Chain) -> anyhow::Result<bool> {
        let changed = {
            let mut i = self.lock();
            if other.pubkey() != Some(i.identity.pubkey_hex().as_str()) {
                anyhow::bail!("that chain belongs to another account");
            }
            i.chain.merge(other)?
        };
        if changed {
            self.save()?;
        }
        Ok(changed)
    }

    /// The bundle another device needs to become this account.
    pub fn bundle(&self) -> cued_proto::AccountBundle {
        let i = self.lock();
        cued_proto::AccountBundle { secret_hex: i.identity.secret_hex(), chain: i.chain.clone() }
    }

    /// Replaces this desktop's account with `bundle` (the phone's), binding this desktop into it. The old key is kept on disk.
    pub fn adopt(&self, bundle: cued_proto::AccountBundle, my_name: &str) -> anyhow::Result<()> {
        let identity = Identity::from_secret_hex(&bundle.secret_hex)?;
        let mut chain = bundle.chain;
        chain.validate()?;
        if chain.pubkey() != Some(identity.pubkey_hex().as_str()) {
            anyhow::bail!("chain and key do not match");
        }
        self.backup_current()?;
        {
            let mut i = self.lock();
            let me = devhash(&identity.pubkey_hex(), &i.salt);
            if !chain.devices().iter().any(|d| d["devhash"].as_str() == Some(me.as_str())) {
                chain.append(&identity, "bind_device", json!({"devhash": me, "name": my_name, "kind": "desktop"}), cued_proto::nostr::now_secs())?;
            }
            i.identity = identity.clone();
            i.chain = chain;
        }
        crate::identity::save(&self.dir.join("identity.json"), &identity)?;
        self.save()
    }

    /// A brand-new account; the old key and chain are kept as `identity.<time>.json.bak`.
    pub fn renew(&self, name: &str, my_name: &str) -> anyhow::Result<()> {
        self.backup_current()?;
        let identity = Identity::generate();
        let now = cued_proto::nostr::now_secs();
        let mut chain = Chain::create(&identity, name, now);
        {
            let mut i = self.lock();
            let me = devhash(&identity.pubkey_hex(), &i.salt);
            chain.append(&identity, "bind_device", json!({"devhash": me, "name": my_name, "kind": "desktop"}), now)?;
            i.identity = identity.clone();
            i.chain = chain;
        }
        crate::identity::save(&self.dir.join("identity.json"), &identity)?;
        self.save()
    }

    fn backup_current(&self) -> anyhow::Result<()> {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let i = self.lock();
        let bak = self.dir.join(format!("identity.{stamp}.json.bak"));
        crate::identity::save(&bak, &i.identity)?;
        std::fs::write(self.dir.join(format!("account.{stamp}.json.bak")), serde_json::to_vec_pretty(&i.chain)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_settings_and_adopt() {
        let dir = tempfile::tempdir().unwrap();
        let a = AccountStore::open(dir.path(), "Laptop").unwrap();
        assert_eq!(a.summary()["devices"].as_array().unwrap().len(), 1);
        assert!(!a.relay_enabled());
        let s = a.update_shared(&json!({"relayEnabled": true, "theme": {"accent": "#ff0000", "bad": "red"}, "junk": 1})).unwrap();
        assert_eq!(s["relayEnabled"], true);
        assert_eq!(s["theme"]["accent"], "#ff0000");
        assert_eq!(s["theme"]["bg"], "#0e0f13");
        assert!(s["theme"].get("bad").is_none());
        assert!(a.update_shared(&json!({"junk": 1})).is_err());
        // Reopen: persisted.
        let b = AccountStore::open(dir.path(), "Laptop").unwrap();
        assert!(b.relay_enabled());
        assert_eq!(b.pubkey_hex(), a.pubkey_hex());
        // Adopt a phone's account.
        let phone = Identity::generate();
        let mut chain = Chain::create(&phone, "Idris", 1);
        chain.append(&phone, "bind_device", json!({"devhash": "ab".repeat(32), "name": "Pixel", "kind": "phone"}), 2).unwrap();
        b.adopt(cued_proto::AccountBundle { secret_hex: phone.secret_hex(), chain }, "Laptop").unwrap();
        assert_eq!(b.pubkey_hex(), phone.pubkey_hex());
        assert_eq!(b.summary()["devices"].as_array().unwrap().len(), 2);
        assert_eq!(b.summary()["name"], "Idris");
        assert!(dir.path().read_dir().unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".json.bak")));
        let c = AccountStore::open(dir.path(), "Laptop").unwrap();
        assert_eq!(c.pubkey_hex(), phone.pubkey_hex());
        assert!(c.unbind_device(&c.my_devhash()).is_err());
        c.unbind_device(&"ab".repeat(32)).unwrap();
        assert_eq!(c.summary()["devices"].as_array().unwrap().len(), 1);
    }
}
