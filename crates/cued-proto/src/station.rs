//! Stations: `cued://station?…` follow links and the signed state a host
//! publishes as a Nostr kind-30777 event. Mirrors `core/station/Station.kt`.

use crate::link::{build_query, get, is_hex, parse_query};
use crate::{Error, Result, SCHEME};

/// Parameterised-replaceable kind: a relay keeps only the newest per author + `d` tag.
pub const KIND: u32 = 30777;
pub const D_TAG: &str = "cued";
pub const STATUS_ON_AIR: &str = "on_air";
pub const STATUS_PAUSED: &str = "paused";
pub const STATUS_OFF: &str = "off";

pub fn tags() -> Vec<Vec<String>> {
    vec![vec!["d".into(), D_TAG.into()], vec!["t".into(), "cued-station".into()]]
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct StationTrack {
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default, rename = "durationMs")]
    pub duration_ms: i64,
    /// Source link listeners can download from; null means search by title and artist.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    /// Stable key on the host's side, so listeners can tell "same track again" from "moved on".
    #[serde(default)]
    pub key: String,
}

impl StationTrack {
    /// What a listener's downloader is given.
    pub fn source(&self) -> String {
        self.link.clone().unwrap_or_else(|| format!("{} {}", self.artist, self.title))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StationState {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default = "off")]
    pub status: String,
    #[serde(default)]
    pub now: Option<StationTrack>,
    /// Wall-clock millis at which `now` was at position 0 (on air).
    #[serde(default, rename = "startedAt")]
    pub started_at: i64,
    /// Position while paused.
    #[serde(default, rename = "positionMs")]
    pub position_ms: i64,
    #[serde(default)]
    pub next: Vec<StationTrack>,
    /// Monotonic per host; listeners ignore anything older than what they have.
    #[serde(default)]
    pub seq: i64,
}

fn one() -> u32 {
    1
}
fn off() -> String {
    STATUS_OFF.to_string()
}

impl Default for StationState {
    fn default() -> Self {
        Self { v: 1, name: String::new(), status: off(), now: None, started_at: 0, position_ms: 0, next: vec![], seq: 0 }
    }
}

impl StationState {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("station state serialises")
    }
    pub fn decode(content: &str) -> Result<Self> {
        Ok(serde_json::from_str(content)?)
    }
    /// Where the host is in `now` at wall-clock `now_ms`.
    pub fn expected_position_ms(&self, now_ms: i64) -> i64 {
        if self.status == STATUS_ON_AIR {
            (now_ms - self.started_at).max(0)
        } else {
            self.position_ms
        }
    }
}

/// `cued://station?p=<pubkey hex>&n=<name>&r=<relay,relay>`
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct StationLink {
    pub pubkey: String,
    pub name: String,
    pub relays: Vec<String>,
}

impl StationLink {
    fn prefix() -> String {
        format!("{SCHEME}://station?")
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
        let rest = t.strip_prefix(Self::prefix().as_str()).ok_or(Error::NotALink("station"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_json_matches_phone_field_names() {
        let s = StationState {
            name: "Idris".into(),
            status: STATUS_ON_AIR.into(),
            now: Some(StationTrack { title: "Afterlife".into(), artist: "Evanescence".into(), key: "k1".into(), ..Default::default() }),
            started_at: 1_700_000_000_000,
            seq: 3,
            ..Default::default()
        };
        let json = s.encode();
        // kotlinx.serialization with encodeDefaults = true writes every field, in declaration order.
        assert_eq!(
            json,
            r#"{"v":1,"name":"Idris","status":"on_air","now":{"title":"Afterlife","artist":"Evanescence","album":"","durationMs":0,"link":null,"cover":null,"key":"k1"},"startedAt":1700000000000,"positionMs":0,"next":[],"seq":3}"#
        );
        assert_eq!(StationState::decode(&json).unwrap(), s);
        assert_eq!(s.expected_position_ms(1_700_000_010_000), 10_000);
        // Unknown keys from a newer phone are ignored.
        assert!(StationState::decode(r#"{"v":2,"status":"off","future":true}"#).is_ok());
    }

    #[test]
    fn station_link() {
        let pk = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let l = StationLink { pubkey: pk.into(), name: "Late Nights".into(), relays: vec!["wss://nos.lol".into(), "ws://10.0.0.2:8770/relay".into()] };
        let s = l.encode();
        assert_eq!(s, format!("cued://station?p={pk}&n=Late%20Nights&r=wss%3A%2F%2Fnos.lol%2Cws%3A%2F%2F10.0.0.2%3A8770%2Frelay"));
        assert_eq!(StationLink::decode(&s).unwrap(), l);
        assert!(StationLink::decode("cued://station?p=zz").is_err());
        assert!(StationLink::decode(&format!("cued://station?p={}", pk.to_uppercase())).unwrap().pubkey == pk);
    }
}
