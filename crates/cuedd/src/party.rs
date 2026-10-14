//! The shared queue ("party"): a friend on the same Wi-Fi asks this desktop to
//! search its library, queue a song or nudge playback. Every message is a signed
//! envelope (`docs/protocol.md` §13) so only people in the friends list are
//! served, and the guest can tell the answer really came from this desktop.
//!
//! Discovery is a UDP beacon on port 8772: a guest broadcasts `CUED-PARTY?`,
//! every host on the LAN answers with its name, public key and URL.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::player::QueueItem;
use crate::state::{AppState, UiEvent};

pub const BEACON_PORT: u16 = 8772;
pub const BEACON_QUERY: &str = "CUED-PARTY?";
pub const BEACON_PREFIX: &str = "CUED-PARTY ";
pub const MAX_SKEW_SEC: i64 = 90;
pub const MAX_RESULTS: i64 = 12;
pub const MAX_QUERY: usize = 80;

pub const OPS: [&str; 7] = ["hello", "search", "queue", "now", "skip", "pause", "play"];
pub const CONTROL_OPS: [&str; 3] = ["skip", "pause", "play"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default = "one")]
    pub v: u32,
    pub pubkey: String,
    pub ts: i64,
    pub nonce: String,
    pub op: String,
    #[serde(default)]
    pub arg: serde_json::Value,
    #[serde(default = "yes")]
    pub ok: bool,
    #[serde(default)]
    pub sig: String,
}

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PartyTrack {
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(rename = "durationMs", default)]
    pub duration_ms: i64,
}

impl From<&QueueItem> for PartyTrack {
    fn from(q: &QueueItem) -> Self {
        Self { sha256: q.sha256.clone(), id: 0, title: q.title.clone(), artist: q.artist.clone(), album: q.album.clone(), duration_ms: q.duration_ms }
    }
}

/// Canonical JSON: keys sorted, no whitespace. Same bytes as the phone's `Party.canonical`.
pub fn canonical(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys.into_iter().map(|k| format!("{}:{}", serde_json::to_string(k).unwrap_or_default(), canonical(&m[k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// BIP-340 style tagged hash over every field but the signature.
pub fn digest(e: &Envelope) -> [u8; 32] {
    let body = format!("cued-party|{}|{}|{}|{}|{}|{}|{}", e.v, e.pubkey, e.ts, e.nonce, e.op, if e.ok { 1 } else { 0 }, canonical(&e.arg));
    let tag = Sha256::digest(b"CUEd/party");
    let mut h = Sha256::new();
    h.update(tag);
    h.update(tag);
    h.update(body.as_bytes());
    h.finalize().into()
}

pub fn verify(e: &Envelope) -> bool {
    e.v == 1 && OPS.contains(&e.op.as_str()) && e.nonce.len() == 16 && e.pubkey.len() == 64 && cued_proto::nostr::verify_schnorr(&hex::encode(digest(e)), &e.pubkey, &e.sig)
}

pub fn sign(id: &cued_proto::Identity, op: &str, arg: serde_json::Value, ok: bool, nonce: &str, ts: i64) -> Envelope {
    let mut e = Envelope { v: 1, pubkey: id.pubkey_hex(), ts, nonce: nonce.to_string(), op: op.to_string(), arg, ok, sig: String::new() };
    let aux: [u8; 32] = rand::random();
    e.sig = hex::encode(id.sign_raw(&digest(&e), &aux));
    e
}

#[cfg(test)]
pub fn new_nonce() -> String {
    hex::encode(rand::random::<[u8; 8]>())
}

fn now_secs() -> i64 {
    cued_proto::nostr::now_secs()
}

/// Remembers nonces for a while so a captured request cannot be replayed.
#[derive(Default)]
pub struct NonceCache {
    seen: HashMap<String, Instant>,
}

impl NonceCache {
    pub fn seen_before(&mut self, nonce: &str) -> bool {
        let ttl = Duration::from_secs((MAX_SKEW_SEC * 2) as u64);
        self.seen.retain(|_, t| t.elapsed() < ttl);
        self.seen.insert(nonce.to_string(), Instant::now()).is_some()
    }
}

pub enum Gate {
    Ok(String),
    Bad(String),
}

/// Signature, skew, replay, friendship. Returns the friend's display name on success.
pub fn gate(s: &AppState, e: &Envelope) -> Gate {
    if !verify(e) {
        return Gate::Bad("bad signature".into());
    }
    if (now_secs() - e.ts).abs() > MAX_SKEW_SEC {
        return Gate::Bad(format!("clock is off by more than {MAX_SKEW_SEC}s"));
    }
    if s.party_nonces.lock().unwrap_or_else(|p| p.into_inner()).seen_before(&e.nonce) {
        return Gate::Bad("replayed request".into());
    }
    match s.db.friend(&e.pubkey) {
        Ok(Some(f)) if f.status == "friends" => Gate::Ok(if f.name.trim().is_empty() { f.npub.chars().take(12).collect() } else { f.name }),
        Ok(Some(_)) => Gate::Bad("friend request still pending".into()),
        _ => Gate::Bad("not a friend of this desktop".into()),
    }
}

fn now_payload(s: &AppState) -> serde_json::Value {
    let (current, queue, pos, playing) = s.player.snapshot();
    serde_json::json!({
        "current": current.as_ref().map(PartyTrack::from),
        "positionMs": (pos * 1000.0) as i64,
        "playing": playing,
        "queue": queue.iter().take(10).map(PartyTrack::from).collect::<Vec<_>>(),
        "queueLength": queue.len(),
    })
}

/// Answers one request. The reply echoes the request's nonce and is signed with this desktop's key.
pub fn handle(s: &AppState, e: &Envelope) -> Envelope {
    let id = s.identity();
    let reply = |ok: bool, arg: serde_json::Value| sign(&id, &e.op, arg, ok, &e.nonce, now_secs());
    let err = |m: &str| reply(false, serde_json::json!({ "error": m }));
    let settings = s.settings();
    if !settings.party_enabled {
        return err("this desktop is not hosting a party");
    }
    let who = match gate(s, e) {
        Gate::Ok(name) => name,
        Gate::Bad(m) => return err(&m),
    };
    if CONTROL_OPS.contains(&e.op.as_str()) && !settings.party_controls {
        return err("the host only lets guests queue songs");
    }
    let announce = |what: String| {
        let _ = s.events.send(UiEvent::Party { who: who.clone(), what });
    };
    match e.op.as_str() {
        "hello" => reply(true, serde_json::json!({ "name": settings.name, "pubkey": id.pubkey_hex(), "controls": settings.party_controls, "now": now_payload(s) })),
        "now" => reply(true, now_payload(s)),
        "search" => {
            let q: String = e.arg.get("q").and_then(|v| v.as_str()).unwrap_or("").chars().take(MAX_QUERY).collect();
            if q.trim().is_empty() {
                return reply(true, serde_json::json!([]));
            }
            match s.db.tracks(&q, None, 0, MAX_RESULTS) {
                Ok((tracks, _)) => reply(true, serde_json::json!(tracks.iter().map(|t| PartyTrack { sha256: t.sha256.clone(), id: 0, title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(), duration_ms: t.duration_ms }).collect::<Vec<_>>())),
                Err(e) => err(&e.to_string()),
            }
        }
        "queue" => {
            let sha = e.arg.get("sha256").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
            match s.db.track(&sha) {
                Ok(Some(t)) => {
                    if !s.player.enabled() {
                        return err("the desktop's audio engine is off");
                    }
                    let (current, _, _, _) = s.player.snapshot();
                    if current.is_none() {
                        if let Err(e) = s.player.load(&t) {
                            return err(&format!("could not play: {e}"));
                        }
                        announce(format!("started {} – {}", t.artist, t.title));
                    } else {
                        s.player.enqueue(&t);
                        announce(format!("queued {} – {}", t.artist, t.title));
                    }
                    reply(true, serde_json::json!({ "queued": true, "queueLength": s.player.queue().len() }))
                }
                Ok(None) => err("no such track on this desktop"),
                Err(e) => err(&e.to_string()),
            }
        }
        "skip" => {
            let advanced = s.player.next();
            announce("skipped".into());
            reply(true, serde_json::json!({ "advanced": advanced, "now": now_payload(s) }))
        }
        "pause" => {
            s.player.pause();
            announce("paused".into());
            reply(true, now_payload(s))
        }
        "play" => {
            s.player.play();
            announce("resumed".into());
            reply(true, now_payload(s))
        }
        _ => err("unknown op"),
    }
}

/// The beacon: answers `CUED-PARTY?` broadcasts while a party is on.
pub fn spawn_beacon(state: Arc<AppState>, shutdown: CancellationToken) {
    tokio::spawn(async move {
        let sock = match tokio::net::UdpSocket::bind(("0.0.0.0", BEACON_PORT)).await {
            Ok(s) => s,
            Err(e) => {
                warn!("party beacon: cannot bind udp {BEACON_PORT}: {e}");
                return;
            }
        };
        let _ = sock.set_broadcast(true);
        info!("party beacon listening on udp {BEACON_PORT}");
        let mut buf = [0u8; 256];
        loop {
            let (n, from) = tokio::select! {
                _ = shutdown.cancelled() => break,
                r = sock.recv_from(&mut buf) => match r { Ok(x) => x, Err(_) => continue },
            };
            if &buf[..n] != BEACON_QUERY.as_bytes() || !state.settings().party_enabled {
                continue;
            }
            if !crate::state::is_lan(from.ip()) {
                continue;
            }
            let Some(url) = state.lan_urls().into_iter().next() else { continue };
            let answer = format!("{BEACON_PREFIX}{}", serde_json::json!({ "name": state.settings().name, "pubkey": state.identity().pubkey_hex(), "url": url }));
            let _ = sock.send_to(answer.as_bytes(), from).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_and_tamper() {
        let id = cued_proto::Identity::generate();
        let e = sign(&id, "search", serde_json::json!({ "q": "daft punk" }), true, &new_nonce(), now_secs());
        assert!(verify(&e));
        let text = serde_json::to_string(&e).unwrap();
        let back: Envelope = serde_json::from_str(&text).unwrap();
        assert!(verify(&back));
        let mut t = back.clone();
        t.op = "skip".into();
        assert!(!verify(&t));
        let mut t = back.clone();
        t.arg = serde_json::json!({ "q": "other" });
        assert!(!verify(&t));
        let mut t = back.clone();
        t.ok = false;
        assert!(!verify(&t));
        let mut t = back;
        t.ts += 1;
        assert!(!verify(&t));
    }

    #[test]
    fn canonical_is_order_independent() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":"x","c":[true,2.5]}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"c":[true,2.5],"a":"x","b":1}"#).unwrap();
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(canonical(&a), r#"{"a":"x","b":1,"c":[true,2.5]}"#);
    }

    #[test]
    fn digest_matches_phone_vector() {
        // Same envelope hashed by the phone's `Party.digest` (fields: v, pubkey, ts, nonce, op, ok, canonical(arg)).
        let e = Envelope { v: 1, pubkey: "ab".repeat(32), ts: 1_700_000_000, nonce: "0011223344556677".into(), op: "queue".into(), arg: serde_json::json!({ "title": "T", "sha256": "ab" }), ok: true, sig: String::new() };
        let body = format!("cued-party|1|{}|1700000000|0011223344556677|queue|1|{{\"sha256\":\"ab\",\"title\":\"T\"}}", "ab".repeat(32));
        let tag = Sha256::digest(b"CUEd/party");
        let mut h = Sha256::new();
        h.update(tag);
        h.update(tag);
        h.update(body.as_bytes());
        let expect: [u8; 32] = h.finalize().into();
        assert_eq!(digest(&e), expect);
    }

    #[test]
    fn nonce_cache_catches_replay() {
        let mut c = NonceCache::default();
        assert!(!c.seen_before("a"));
        assert!(c.seen_before("a"));
        assert!(!c.seen_before("b"));
    }
}
