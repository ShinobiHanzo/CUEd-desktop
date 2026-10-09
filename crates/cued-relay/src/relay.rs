//! The relay itself and the per-socket [`Connection`] that speaks NIP-01.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cued_proto::nostr::{self, ClientMessage};
use cued_proto::Event;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::filter::{any_matches, Filter};
use crate::{store, MAX_EVENT_BYTES, MAX_FUTURE_SECS, MAX_SUBSCRIPTIONS, MAX_SUB_ID_LEN};

/// Outcome of offering an event to the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accept {
    /// Verified and written to the database; live subscribers were notified.
    Stored,
    /// Verified but not stored: already known, or older than the stored
    /// replaceable event. The string is the NIP-01 `OK` message (`duplicate: …`).
    Duplicate(String),
    /// Refused. The string is the NIP-01 `OK` message (`invalid: …`, `error: …`, `blocked: …`).
    Rejected(String),
    /// Verified, ephemeral kind: fanned out to live subscribers, never stored.
    Ephemeral,
}

impl Accept {
    /// `true` for the `OK` frame's accepted flag.
    pub fn is_ok(&self) -> bool {
        !matches!(self, Accept::Rejected(_))
    }

    /// The message carried in the `OK` frame.
    pub fn message(&self) -> &str {
        match self {
            Accept::Stored | Accept::Ephemeral => "",
            Accept::Duplicate(m) | Accept::Rejected(m) => m,
        }
    }
}

/// A snapshot of the relay's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RelayStats {
    /// Events in the database.
    pub events: u64,
    /// Live [`Connection`]s.
    pub connections: usize,
    /// Open subscriptions across all connections.
    pub subscriptions: usize,
    /// Serialised size of the stored events, in bytes.
    pub bytes: u64,
}

/// A broadcast of an accepted event, numbered so a subscription opened after
/// the event was stored does not receive it twice (once from the query, once live).
#[derive(Clone)]
struct Published {
    seq: u64,
    event: Arc<Event>,
}

/// The relay. Construct with [`Relay::open`], share as `Arc<Relay>`.
pub struct Relay {
    db: Mutex<rusqlite::Connection>,
    /// Internal fan-out to connections.
    live: broadcast::Sender<Published>,
    /// Public fan-out for the daemon (see [`Relay::subscribe_all`]).
    all: broadcast::Sender<Event>,
    seq: AtomicU64,
    connections: AtomicUsize,
    subscriptions: AtomicUsize,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay").field("stats", &self.stats()).finish()
    }
}

impl Relay {
    /// Opens the relay on a SQLite file (created with its parent directory when
    /// missing) or, with `None`, in memory.
    pub fn open(path: Option<&Path>) -> anyhow::Result<Arc<Relay>> {
        let db = store::open(path)?;
        let (live, _) = broadcast::channel(1024);
        let (all, _) = broadcast::channel(1024);
        Ok(Arc::new(Relay {
            db: Mutex::new(db),
            live,
            all,
            seq: AtomicU64::new(0),
            connections: AtomicUsize::new(0),
            subscriptions: AtomicUsize::new(0),
        }))
    }

    fn db(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new client connection; one per websocket.
    pub fn connect(self: &Arc<Self>) -> Connection {
        self.connections.fetch_add(1, Ordering::Relaxed);
        Connection { relay: Arc::clone(self), rx: self.live.subscribe(), subs: HashMap::new(), pending: VecDeque::new() }
    }

    /// Every event the relay accepts (stored or ephemeral), as it is accepted.
    /// A slow receiver sees `RecvError::Lagged` and keeps going.
    pub fn subscribe_all(&self) -> broadcast::Receiver<Event> {
        self.all.subscribe()
    }

    /// Runs the full acceptance pipeline: size, id, signature, timestamp, then
    /// storage with replaceable semantics, then live fan-out. Used by
    /// connections for `EVENT` frames and by the daemon's upstream mirror.
    pub fn publish(&self, ev: &Event) -> Accept {
        let size = serde_json::to_string(ev).map(|s| s.len()).unwrap_or(usize::MAX);
        if size > MAX_EVENT_BYTES {
            return Accept::Rejected("blocked: event too large".into());
        }
        if !is_hex(&ev.id, 64) || !is_hex(&ev.pubkey, 64) || !is_hex(&ev.sig, 128) {
            return Accept::Rejected("invalid: id, pubkey and sig must be lowercase hex".into());
        }
        if nostr::id(&ev.pubkey, ev.created_at, ev.kind, &ev.tags, &ev.content) != ev.id {
            return Accept::Rejected("invalid: id mismatch".into());
        }
        if !nostr::verify_schnorr(&ev.id, &ev.pubkey, &ev.sig) {
            return Accept::Rejected("invalid: bad signature".into());
        }
        let now = nostr::now_secs();
        if ev.created_at > now + MAX_FUTURE_SECS {
            return Accept::Rejected("error: created_at too far in the future".into());
        }

        let db = self.db();
        if ev.is_ephemeral() {
            self.fan_out(ev);
            return Accept::Ephemeral;
        }
        match store::has_event(&db, &ev.id) {
            Ok(true) => return Accept::Duplicate("duplicate: already have this event".into()),
            Ok(false) => {}
            Err(e) => return Accept::Rejected(format!("error: database: {e}")),
        }
        let replace_key = if ev.is_replaceable() {
            Some(None)
        } else if ev.is_param_replaceable() {
            Some(Some(ev.tag("d").unwrap_or("")))
        } else {
            None
        };
        let result = (|| -> rusqlite::Result<Accept> {
            if let Some(d_tag) = replace_key {
                if let Some(cur) = store::latest(&db, ev.kind, &ev.pubkey, d_tag)? {
                    let newer = ev.created_at > cur.created_at || (ev.created_at == cur.created_at && ev.id < cur.id);
                    if !newer {
                        return Ok(Accept::Duplicate("duplicate: older than the stored replaceable event".into()));
                    }
                }
                store::delete_replaced(&db, ev.kind, &ev.pubkey, d_tag)?;
            }
            store::insert(&db, ev, size, now)?;
            Ok(Accept::Stored)
        })();
        match result {
            Ok(Accept::Stored) => {
                self.fan_out(ev);
                Accept::Stored
            }
            Ok(other) => other,
            Err(e) => Accept::Rejected(format!("error: database: {e}")),
        }
    }

    /// Caller holds the db lock, so `seq` and the stored state move together.
    fn fan_out(&self, ev: &Event) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.live.send(Published { seq, event: Arc::new(ev.clone()) });
        let _ = self.all.send(ev.clone());
    }

    /// Stored events matching any of the filters (JSON filter objects as in a
    /// `REQ`), newest first. Filters that fail to parse are skipped.
    pub fn query(&self, filters: &[Value]) -> Vec<Event> {
        let parsed: Vec<Filter> = filters.iter().filter_map(|f| Filter::parse(f).ok()).collect();
        self.query_filters(&parsed).0
    }

    /// Runs the filters under one lock and returns the sequence number at that
    /// instant, so live pushes numbered at or below it are already in the result.
    fn query_filters(&self, filters: &[Filter]) -> (Vec<Event>, u64) {
        let db = self.db();
        let seq = self.seq.load(Ordering::SeqCst);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for f in filters {
            match store::query(&db, f) {
                Ok(events) => {
                    for ev in events {
                        if seen.insert(ev.id.clone()) {
                            out.push(ev);
                        }
                    }
                }
                Err(e) => tracing::warn!("relay query failed: {e}"),
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        (out, seq)
    }

    /// Newest stored event of `kind` by `pubkey`; `d_tag` pins the `d` tag for
    /// parameterised-replaceable kinds (`None` = any).
    pub fn latest_by_author(&self, kind: u32, pubkey: &str, d_tag: Option<&str>) -> Option<Event> {
        store::latest(&self.db(), kind, pubkey, d_tag).ok().flatten()
    }

    pub fn stats(&self) -> RelayStats {
        let db = self.db();
        RelayStats {
            events: store::count(&db).unwrap_or(0),
            bytes: store::bytes(&db).unwrap_or(0),
            connections: self.connections.load(Ordering::Relaxed),
            subscriptions: self.subscriptions.load(Ordering::Relaxed),
        }
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

struct Subscription {
    filters: Vec<Filter>,
    /// Live events with a sequence number at or below this were covered by the
    /// initial query.
    since_seq: u64,
}

/// One client (one websocket). Feed it inbound text frames with
/// [`Connection::handle`], send back what it returns, and forward whatever
/// [`Connection::next_push`] yields. Dropping it closes its subscriptions.
pub struct Connection {
    relay: Arc<Relay>,
    rx: broadcast::Receiver<Published>,
    subs: HashMap<String, Subscription>,
    pending: VecDeque<String>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").field("subscriptions", &self.subs.len()).finish()
    }
}

impl Connection {
    /// The relay this connection belongs to.
    pub fn relay(&self) -> &Arc<Relay> {
        &self.relay
    }

    /// Open subscription ids.
    pub fn subscription_ids(&self) -> impl Iterator<Item = &str> {
        self.subs.keys().map(String::as_str)
    }

    /// Processes one inbound NIP-01 frame and returns the frames to send back,
    /// in order. Never panics on garbage: unparsable input yields a `NOTICE`.
    pub fn handle(&mut self, text: &str) -> Vec<String> {
        match nostr::parse_client_message(text) {
            ClientMessage::Event(ev) => {
                let accept = self.relay.publish(&ev);
                vec![nostr::ok_message(&ev.id, accept.is_ok(), accept.message())]
            }
            ClientMessage::Req { sub_id, filters } => self.req(sub_id, &filters),
            ClientMessage::Close(sub_id) => {
                self.remove_sub(&sub_id);
                Vec::new()
            }
            ClientMessage::Unknown(_) => vec![nostr::notice_message("error: could not parse message")],
        }
    }

    fn req(&mut self, sub_id: String, filters: &[Value]) -> Vec<String> {
        if sub_id.is_empty() || sub_id.len() > MAX_SUB_ID_LEN {
            return vec![nostr::closed_message(&sub_id, "error: subscription id must be 1 to 64 characters")];
        }
        if filters.is_empty() {
            return vec![nostr::closed_message(&sub_id, "error: a REQ needs at least one filter")];
        }
        let parsed = match Filter::parse_all(filters) {
            Ok(p) => p,
            Err(e) => return vec![nostr::closed_message(&sub_id, &format!("error: invalid filter: {e}"))],
        };
        if !self.subs.contains_key(&sub_id) && self.subs.len() >= MAX_SUBSCRIPTIONS {
            return vec![nostr::closed_message(&sub_id, "error: too many subscriptions")];
        }
        let (events, seq) = self.relay.query_filters(&parsed);
        if self.subs.insert(sub_id.clone(), Subscription { filters: parsed, since_seq: seq }).is_none() {
            self.relay.subscriptions.fetch_add(1, Ordering::Relaxed);
        }
        let mut out: Vec<String> = events.iter().map(|ev| nostr::relay_event_message(&sub_id, ev)).collect();
        out.push(nostr::eose_message(&sub_id));
        out
    }

    fn remove_sub(&mut self, sub_id: &str) {
        if self.subs.remove(sub_id).is_some() {
            self.relay.subscriptions.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Waits for the next live frame for this connection: an `["EVENT", sub_id,
    /// event]` for each subscription a newly accepted event matches, or an
    /// occasional `NOTICE` when this connection fell behind and missed events.
    /// Returns `None` only if the relay was dropped. Cancel-safe, so it is fine
    /// inside `tokio::select!`.
    pub async fn next_push(&mut self) -> Option<String> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some(frame);
            }
            match self.rx.recv().await {
                Ok(published) => {
                    let ev: &Event = &published.event;
                    let mut ids: Vec<&String> = self
                        .subs
                        .iter()
                        .filter(|(_, s)| published.seq > s.since_seq && any_matches(&s.filters, ev))
                        .map(|(id, _)| id)
                        .collect();
                    ids.sort();
                    self.pending.extend(ids.into_iter().map(|id| nostr::relay_event_message(id, ev)));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    return Some(nostr::notice_message(&format!("warning: this connection fell behind; {n} events were not delivered")));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.relay.connections.fetch_sub(1, Ordering::Relaxed);
        if !self.subs.is_empty() {
            self.relay.subscriptions.fetch_sub(self.subs.len(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cued_proto::nostr::{parse_relay_message, RelayMessage};
    use cued_proto::Identity;
    use serde_json::json;

    fn relay() -> Arc<Relay> {
        Relay::open(None).unwrap()
    }

    fn event_frame(ev: &Event) -> String {
        nostr::event_message(ev)
    }

    fn parse(frames: &[String]) -> Vec<RelayMessage> {
        frames.iter().map(|f| parse_relay_message(f)).collect()
    }

    #[test]
    fn stores_then_serves_with_eose() {
        let relay = relay();
        let me = Identity::generate();
        let ev = me.sign_now(1, vec![], "hello".into());
        let mut c = relay.connect();
        let ok = parse(&c.handle(&event_frame(&ev)));
        assert_eq!(ok, vec![RelayMessage::Ok { event_id: ev.id.clone(), accepted: true, message: String::new() }]);
        let got = parse(&c.handle(r#"["REQ","s1",{"kinds":[1]}]"#));
        assert_eq!(got, vec![RelayMessage::Event { sub_id: "s1".into(), event: ev.clone() }, RelayMessage::Eose("s1".into())]);
        assert_eq!(relay.stats().events, 1);
        assert_eq!(relay.stats().subscriptions, 1);
        assert_eq!(relay.stats().connections, 1);
        drop(c);
        assert_eq!(relay.stats().subscriptions, 0);
        assert_eq!(relay.stats().connections, 0);
    }

    #[tokio::test]
    async fn live_push_to_second_connection() {
        let relay = relay();
        let me = Identity::generate();
        let mut a = relay.connect();
        let mut b = relay.connect();
        let frames = b.handle(r##"["REQ","live",{"kinds":[1],"#t":["x"]}]"##);
        assert_eq!(parse(&frames), vec![RelayMessage::Eose("live".into())]);
        let ev = me.sign_now(1, vec![vec!["t".into(), "x".into()]], "ping".into());
        let other = me.sign_now(1, vec![vec!["t".into(), "y".into()]], "not for b".into());
        a.handle(&event_frame(&other));
        a.handle(&event_frame(&ev));
        let push = tokio::time::timeout(std::time::Duration::from_secs(2), b.next_push()).await.unwrap().unwrap();
        assert_eq!(parse_relay_message(&push), RelayMessage::Event { sub_id: "live".into(), event: ev.clone() });
        // nothing else queued for b: `other` did not match
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), b.next_push()).await.is_err());
        // and a (no subs) gets nothing
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), a.next_push()).await.is_err());
    }

    #[tokio::test]
    async fn no_double_delivery_for_events_stored_before_req() {
        let relay = relay();
        let me = Identity::generate();
        let mut c = relay.connect();
        let ev = me.sign_now(1, vec![], "before".into());
        assert_eq!(relay.publish(&ev), Accept::Stored);
        let frames = c.handle(r#"["REQ","s",{"kinds":[1]}]"#);
        assert_eq!(frames.len(), 2);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), c.next_push()).await.is_err());
        let later = me.sign_now(1, vec![], "after".into());
        assert_eq!(relay.publish(&later), Accept::Stored);
        let push = c.next_push().await.unwrap();
        assert_eq!(parse_relay_message(&push), RelayMessage::Event { sub_id: "s".into(), event: later });
    }

    #[test]
    fn param_replaceable_keeps_newest() {
        let relay = relay();
        let me = Identity::generate();
        let d = vec![vec!["d".into(), "cued".into()]];
        let now = nostr::now_secs();
        let old = me.sign(30777, d.clone(), "old".into(), now - 10);
        let new = me.sign(30777, d.clone(), "new".into(), now);
        let other_d = me.sign(30777, vec![vec!["d".into(), "other".into()]], "other".into(), now - 5);
        assert_eq!(relay.publish(&new), Accept::Stored);
        assert!(matches!(relay.publish(&old), Accept::Duplicate(m) if m.starts_with("duplicate:")));
        assert_eq!(relay.publish(&other_d), Accept::Stored);
        assert_eq!(relay.publish(&new), Accept::Duplicate("duplicate: already have this event".into()));
        let got = relay.query(&[json!({"kinds":[30777],"authors":[me.pubkey_hex()],"#d":["cued"]})]);
        assert_eq!(got, vec![new.clone()]);
        assert_eq!(relay.latest_by_author(30777, &me.pubkey_hex(), Some("cued")), Some(new.clone()));
        assert_eq!(relay.latest_by_author(30777, &me.pubkey_hex(), Some("other")), Some(other_d));
        assert_eq!(relay.latest_by_author(30777, &me.pubkey_hex(), None), Some(new.clone()));
        assert_eq!(relay.stats().events, 2);

        // newer state replaces the stored one and the old one is gone
        let newer = me.sign(30777, d, "newer".into(), now + 1);
        assert_eq!(relay.publish(&newer), Accept::Stored);
        let got = relay.query(&[json!({"ids":[new.id, newer.id.clone()]})]);
        assert_eq!(got, vec![newer]);
    }

    #[test]
    fn replaceable_keeps_newest_per_author_and_kind() {
        let relay = relay();
        let me = Identity::generate();
        let you = Identity::generate();
        let now = nostr::now_secs();
        let a1 = me.sign(0, vec![], "{\"name\":\"a\"}".into(), now - 1);
        let a2 = me.sign(0, vec![], "{\"name\":\"a2\"}".into(), now);
        let b1 = you.sign(0, vec![], "{\"name\":\"b\"}".into(), now - 1);
        assert_eq!(relay.publish(&a1), Accept::Stored);
        assert_eq!(relay.publish(&b1), Accept::Stored);
        assert_eq!(relay.publish(&a2), Accept::Stored);
        let got = relay.query(&[json!({"kinds":[0]})]);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&a2) && got.contains(&b1));
    }

    #[test]
    fn rejects_bad_signature_id_future_and_size() {
        let relay = relay();
        let me = Identity::generate();
        let mut ev = me.sign_now(1, vec![], "x".into());
        ev.content = "tampered".into();
        assert_eq!(relay.publish(&ev), Accept::Rejected("invalid: id mismatch".into()));
        let mut ev = me.sign_now(1, vec![], "x".into());
        let other = Identity::generate().sign_now(1, vec![], "x".into());
        ev.sig = other.sig;
        assert_eq!(relay.publish(&ev), Accept::Rejected("invalid: bad signature".into()));
        let future = me.sign(1, vec![], "x".into(), nostr::now_secs() + MAX_FUTURE_SECS + 60);
        assert_eq!(relay.publish(&future), Accept::Rejected("error: created_at too far in the future".into()));
        let big = me.sign_now(1, vec![], "z".repeat(MAX_EVENT_BYTES));
        assert_eq!(relay.publish(&big), Accept::Rejected("blocked: event too large".into()));
        let mut ev = me.sign_now(1, vec![], "x".into());
        ev.sig = "XYZ".into();
        assert!(matches!(relay.publish(&ev), Accept::Rejected(m) if m.starts_with("invalid:")));
        assert_eq!(relay.stats().events, 0);

        let mut c = relay.connect();
        let frames = parse(&c.handle(&event_frame(&future)));
        assert_eq!(frames, vec![RelayMessage::Ok { event_id: future.id, accepted: false, message: "error: created_at too far in the future".into() }]);
    }

    #[tokio::test]
    async fn ephemeral_is_pushed_not_stored() {
        let relay = relay();
        let me = Identity::generate();
        let mut c = relay.connect();
        c.handle(r#"["REQ","e",{"kinds":[20001]}]"#);
        let ev = me.sign_now(20001, vec![], "now".into());
        assert_eq!(relay.publish(&ev), Accept::Ephemeral);
        assert_eq!(relay.stats().events, 0);
        let push = c.next_push().await.unwrap();
        assert_eq!(parse_relay_message(&push), RelayMessage::Event { sub_id: "e".into(), event: ev });
    }

    #[test]
    fn limit_since_until_and_ordering() {
        let relay = relay();
        let me = Identity::generate();
        let now = nostr::now_secs();
        let evs: Vec<Event> = (0..10).map(|i| me.sign(1, vec![], format!("{i}"), now - 100 + i)).collect();
        for ev in &evs {
            assert_eq!(relay.publish(ev), Accept::Stored);
        }
        let got = relay.query(&[json!({"kinds":[1],"limit":3})]);
        assert_eq!(got, vec![evs[9].clone(), evs[8].clone(), evs[7].clone()]);
        let got = relay.query(&[json!({"since": now - 97, "until": now - 95})]);
        assert_eq!(got, vec![evs[5].clone(), evs[4].clone(), evs[3].clone()]);
        let got = relay.query(&[json!({"limit":0})]);
        assert!(got.is_empty());
        // two filters are ORed and de-duplicated
        let got = relay.query(&[json!({"ids":[evs[0].id]}), json!({"ids":[evs[0].id, evs[1].id]})]);
        assert_eq!(got, vec![evs[1].clone(), evs[0].clone()]);
        let got = relay.query(&[json!({"authors":[me.pubkey_hex()], "limit": 1}), json!({"kinds":[99]})]);
        assert_eq!(got, vec![evs[9].clone()]);
    }

    #[test]
    fn d_tag_filter() {
        let relay = relay();
        let me = Identity::generate();
        let now = nostr::now_secs();
        let cued = me.sign(30777, vec![vec!["d".into(), "cued".into()], vec!["t".into(), "music".into()]], "s".into(), now);
        let other = me.sign(30777, vec![vec!["d".into(), "other".into()]], "o".into(), now);
        let plain = me.sign(1, vec![vec!["d".into(), "cued".into()]], "p".into(), now);
        for ev in [&cued, &other, &plain] {
            assert_eq!(relay.publish(ev), Accept::Stored);
        }
        let got = relay.query(&[json!({"#d":["cued"]})]);
        assert_eq!(got.len(), 2);
        let got = relay.query(&[json!({"kinds":[30777],"#d":["cued"]})]);
        assert_eq!(got, vec![cued.clone()]);
        let got = relay.query(&[json!({"#d":["cued"],"#t":["music"]})]);
        assert_eq!(got, vec![cued]);
        let got = relay.query(&[json!({"#d":[]})]);
        assert!(got.is_empty());
    }

    #[test]
    fn subscription_limits_and_errors() {
        let relay = relay();
        let mut c = relay.connect();
        for i in 0..MAX_SUBSCRIPTIONS {
            let frames = parse(&c.handle(&format!(r#"["REQ","s{i}",{{}}]"#)));
            assert_eq!(frames, vec![RelayMessage::Eose(format!("s{i}"))]);
        }
        let frames = parse(&c.handle(r#"["REQ","one-too-many",{}]"#));
        assert_eq!(frames, vec![RelayMessage::Closed { sub_id: "one-too-many".into(), message: "error: too many subscriptions".into() }]);
        // re-issuing an existing id is a replacement, not a new subscription
        let frames = parse(&c.handle(r#"["REQ","s0",{"kinds":[1]}]"#));
        assert_eq!(frames, vec![RelayMessage::Eose("s0".into())]);
        assert_eq!(relay.stats().subscriptions, MAX_SUBSCRIPTIONS);
        c.handle(r#"["CLOSE","s0"]"#);
        assert_eq!(relay.stats().subscriptions, MAX_SUBSCRIPTIONS - 1);
        let frames = parse(&c.handle(r#"["REQ","again",{}]"#));
        assert_eq!(frames, vec![RelayMessage::Eose("again".into())]);

        let long = "x".repeat(MAX_SUB_ID_LEN + 1);
        let frames = parse(&c.handle(&format!(r#"["REQ","{long}",{{}}]"#)));
        assert!(matches!(&frames[0], RelayMessage::Closed { message, .. } if message.starts_with("error:")));
        let frames = parse(&c.handle(r#"["REQ","nofilter"]"#));
        assert!(matches!(&frames[0], RelayMessage::Closed { message, .. } if message.starts_with("error:")));
        let frames = parse(&c.handle(r#"["REQ","bad",{"kinds":"1"}]"#));
        assert!(matches!(&frames[0], RelayMessage::Closed { message, .. } if message.starts_with("error: invalid filter")));
        let frames = parse(&c.handle("not json"));
        assert!(matches!(&frames[0], RelayMessage::Notice(m) if m.starts_with("error:")));
        let frames = parse(&c.handle(r#"["AUTH","x"]"#));
        assert!(matches!(&frames[0], RelayMessage::Notice(_)));
        let frames = parse(&c.handle(r#"["EVENT",{"id":"x"}]"#));
        assert!(matches!(&frames[0], RelayMessage::Notice(_)));
    }

    #[test]
    fn on_disk_persists_across_open() {
        let dir = std::env::temp_dir().join(format!("cued-relay-test-{}-{}", std::process::id(), nostr::now_secs()));
        let path = dir.join("nested").join("relay.sqlite");
        let me = Identity::generate();
        let ev = me.sign_now(1, vec![vec!["t".into(), "disk".into()]], "persist".into());
        {
            let relay = Relay::open(Some(&path)).unwrap();
            assert_eq!(relay.publish(&ev), Accept::Stored);
            assert_eq!(relay.stats().events, 1);
            assert!(relay.stats().bytes > 0);
        }
        {
            let relay = Relay::open(Some(&path)).unwrap();
            assert_eq!(relay.stats().events, 1);
            assert_eq!(relay.query(&[json!({"#t":["disk"]})]), vec![ev.clone()]);
            assert_eq!(relay.publish(&ev), Accept::Duplicate("duplicate: already have this event".into()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn subscribe_all_sees_every_accepted_event() {
        let relay = relay();
        let mut all = relay.subscribe_all();
        let me = Identity::generate();
        let stored = me.sign_now(1, vec![], "a".into());
        let eph = me.sign_now(20000, vec![], "b".into());
        relay.publish(&stored);
        relay.publish(&eph);
        assert_eq!(all.recv().await.unwrap(), stored);
        assert_eq!(all.recv().await.unwrap(), eph);
    }

    /// Two connections wired through mpsc channels standing in for websockets:
    /// the shape of the loop the daemon runs per socket.
    #[tokio::test]
    async fn two_connections_over_channels() {
        use tokio::sync::mpsc;

        fn serve(mut conn: Connection, mut inbound: mpsc::Receiver<String>, outbound: mpsc::Sender<String>) {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        msg = inbound.recv() => match msg {
                            Some(text) => {
                                for reply in conn.handle(&text) {
                                    if outbound.send(reply).await.is_err() { return; }
                                }
                            }
                            None => return, // socket closed
                        },
                        push = conn.next_push() => match push {
                            Some(frame) => if outbound.send(frame).await.is_err() { return; },
                            None => return,
                        },
                    }
                }
            });
        }

        let relay = relay();
        let (a_in_tx, a_in_rx) = mpsc::channel(16);
        let (a_out_tx, mut a_out_rx) = mpsc::channel(16);
        let (b_in_tx, b_in_rx) = mpsc::channel(16);
        let (b_out_tx, mut b_out_rx) = mpsc::channel(16);
        serve(relay.connect(), a_in_rx, a_out_tx);
        serve(relay.connect(), b_in_rx, b_out_tx);

        // phone B subscribes to station states
        b_in_tx.send(r##"["REQ","stations",{"kinds":[30777],"#d":["cued"]}]"##.into()).await.unwrap();
        assert_eq!(parse_relay_message(&b_out_rx.recv().await.unwrap()), RelayMessage::Eose("stations".into()));

        // phone A publishes its state
        let me = Identity::generate();
        let state = me.sign_now(30777, vec![vec!["d".into(), "cued".into()]], r#"{"playing":true}"#.into());
        a_in_tx.send(nostr::event_message(&state)).await.unwrap();
        assert_eq!(
            parse_relay_message(&a_out_rx.recv().await.unwrap()),
            RelayMessage::Ok { event_id: state.id.clone(), accepted: true, message: String::new() }
        );
        // B gets it live
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), b_out_rx.recv()).await.unwrap().unwrap();
        assert_eq!(parse_relay_message(&frame), RelayMessage::Event { sub_id: "stations".into(), event: state.clone() });

        // a late joiner gets it from storage
        a_in_tx.send(r#"["REQ","late",{"kinds":[30777]}]"#.into()).await.unwrap();
        assert_eq!(parse_relay_message(&a_out_rx.recv().await.unwrap()), RelayMessage::Event { sub_id: "late".into(), event: state });
        assert_eq!(parse_relay_message(&a_out_rx.recv().await.unwrap()), RelayMessage::Eose("late".into()));

        // closing the "socket" drops the connection and its subscriptions
        drop(b_in_tx);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(relay.stats().connections, 1);
        assert_eq!(relay.stats().subscriptions, 1);
    }
}
