//! `cued://friend?p=<pubkey>&n=<name>&r=<relays>`: what one person hands
//! another over QR or NFC to become friends. Scanning it sends a request;
//! two people scanning each other are friends at once. Only the public key
//! and a name are in it.

use crate::link::{build_query, get, is_hex, parse_query};
use crate::{Error, Result, SCHEME};

/// Nostr kind of a friend request / accept / remove event (parameterised-replaceable).
pub const FRIEND_KIND: u32 = 30778;
pub const FRIEND_TAG: &str = "cued-friend";

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct FriendLink {
    pub pubkey: String,
    pub name: String,
    pub relays: Vec<String>,
}

impl FriendLink {
    fn prefix() -> String {
        format!("{SCHEME}://friend?")
    }

    pub fn encode(&self) -> String {
        let mut q: Vec<(&str, String)> = vec![("p", self.pubkey.clone())];
        if !self.name.trim().is_empty() {
            q.push(("n", self.name.clone()));
        }
        if !self.relays.is_empty() {
            q.push(("r", self.relays.join(",")));
        }
        format!("{}{}", Self::prefix(), build_query(&q))
    }

    pub fn decode(text: &str) -> Result<Self> {
        let t = text.trim();
        let rest = t.strip_prefix(Self::prefix().as_str()).ok_or(Error::NotALink("friend"))?;
        let q = parse_query(rest);
        let p = get(&q, "p").ok_or(Error::Missing("p"))?.to_ascii_lowercase();
        if !is_hex(&p, 64) {
            return Err(Error::Bad("p"));
        }
        Ok(Self {
            pubkey: p,
            name: get(&q, "n").unwrap_or("").to_string(),
            relays: get(&q, "r").map(|r| r.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default(),
        })
    }
}

/// Content of a kind-30778 event.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FriendEvent {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub name: String,
    /// `request`, `accept` or `remove`.
    pub status: String,
    #[serde(default)]
    pub ts: i64,
}

fn one() -> u32 {
    1
}

impl FriendEvent {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("json")
    }
    pub fn decode(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

/// `d` tag of a friend event aimed at `target`.
pub fn d_tag(target: &str) -> String {
    format!("friend:{target}")
}

/// Tags for a friend event aimed at `target`.
pub fn tags(target: &str) -> Vec<Vec<String>> {
    vec![vec!["d".into(), d_tag(target)], vec!["p".into(), target.into()], vec!["t".into(), FRIEND_TAG.into()]]
}

/// Filter for friend events aimed at `me`.
pub fn filter(me: &str, since: Option<i64>) -> serde_json::Value {
    let mut f = serde_json::json!({"kinds": [FRIEND_KIND], "#p": [me]});
    if let Some(s) = since {
        f["since"] = serde_json::json!(s);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_round_trip() {
        let l = FriendLink { pubkey: "ab".repeat(32), name: "Idris".into(), relays: vec!["wss://nos.lol".into()] };
        let s = l.encode();
        assert_eq!(s, format!("cued://friend?p={}&n=Idris&r=wss%3A%2F%2Fnos.lol", "ab".repeat(32)));
        assert_eq!(FriendLink::decode(&s).unwrap(), l);
        assert!(FriendLink::decode("cued://friend?p=zz").is_err());
        assert!(matches!(crate::decode_link(&s), Ok(crate::Link::Friend(_))));
    }

    #[test]
    fn event_shape() {
        let me = "cd".repeat(32);
        assert_eq!(tags(&me)[0], vec!["d".to_string(), format!("friend:{me}")]);
        assert_eq!(filter(&me, None)["#p"][0], me);
        let e = FriendEvent { v: 1, name: "A".into(), status: "request".into(), ts: 5 };
        assert_eq!(FriendEvent::decode(&e.encode()).unwrap(), e);
    }
}
