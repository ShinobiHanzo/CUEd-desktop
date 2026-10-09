//! Friend state transitions and the signed events that carry them.

use cued_proto::friend::{self, FriendEvent};
use cued_proto::nostr::Event;

use crate::api::App;
use crate::db::Friend;
use crate::state::{AppState, UiEvent};

fn publish(s: &AppState, target: &str, status: &str) {
    let content = FriendEvent { v: 1, name: s.account.chain().name(), status: status.into(), ts: cued_proto::nostr::now_secs() }.encode();
    let ev = s.identity().sign_now(friend::FRIEND_KIND, friend::tags(target), content);
    match s.relay.publish(&ev) {
        cued_relay::Accept::Rejected(r) => tracing::warn!("friend event rejected: {r}"),
        _ => tracing::debug!("friend {status} → {}", &target[..8.min(target.len())]),
    }
}

fn settled(s: &AppState, f: &Friend) {
    if f.status == "friends" {
        let _ = s.db.add_follow(&crate::db::Follow { pubkey: f.pubkey.clone(), name: f.name.clone(), relays: f.relays.clone(), added_at: 0 });
        s.stations.poke();
    }
    s.emit(UiEvent::Friends);
}

/// Scanning someone's link: a request, or an acceptance when they already asked.
pub fn request(s: &App, pubkey: &str, name: &str, relays: &[String]) -> anyhow::Result<Friend> {
    let existing = s.db.friend(pubkey)?;
    let f = match existing.as_ref().map(|f| f.status.as_str()) {
        Some("friends") => existing.unwrap(),
        Some("pending_in") => {
            let f = s.db.set_friend(pubkey, name, "friends", relays)?;
            publish(s, pubkey, "accept");
            f
        }
        _ => {
            let f = s.db.set_friend(pubkey, name, "pending_out", relays)?;
            publish(s, pubkey, "request");
            f
        }
    };
    settled(s, &f);
    Ok(f)
}

pub fn accept(s: &App, pubkey: &str) -> anyhow::Result<Option<Friend>> {
    let Some(existing) = s.db.friend(pubkey)? else { return Ok(None) };
    if existing.status == "friends" {
        return Ok(Some(existing));
    }
    let f = s.db.set_friend(pubkey, "", "friends", &[])?;
    publish(s, pubkey, "accept");
    settled(s, &f);
    Ok(Some(f))
}

pub fn remove(s: &App, pubkey: &str) -> anyhow::Result<bool> {
    let removed = s.db.remove_friend(pubkey)?;
    if removed {
        publish(s, pubkey, "remove");
        s.emit(UiEvent::Friends);
    }
    Ok(removed)
}

/// A kind-30778 event aimed at us arrived (from the embedded relay or upstream).
pub fn on_event(s: &AppState, ev: &Event) {
    if ev.kind != friend::FRIEND_KIND || ev.tag("p") != Some(s.identity().pubkey_hex().as_str()) || ev.pubkey == s.identity().pubkey_hex() {
        return;
    }
    let Ok(fe) = FriendEvent::decode(&ev.content) else { return };
    let from = &ev.pubkey;
    let existing = s.db.friend(from).ok().flatten();
    let result = match (fe.status.as_str(), existing.as_ref().map(|f| f.status.as_str())) {
        ("request", Some("pending_out")) | ("request", Some("friends")) => {
            // They asked while we were asking: that is a match.
            let f = s.db.set_friend(from, &fe.name, "friends", &[]);
            if existing.map(|f| f.status != "friends").unwrap_or(true) {
                publish(s, from, "accept");
            }
            f
        }
        ("request", _) => s.db.set_friend(from, &fe.name, "pending_in", &[]),
        ("accept", Some("pending_out")) | ("accept", Some("friends")) => s.db.set_friend(from, &fe.name, "friends", &[]),
        ("remove", Some(_)) => {
            let _ = s.db.remove_friend(from);
            s.emit(UiEvent::Friends);
            return;
        }
        _ => return,
    };
    match result {
        Ok(f) => {
            tracing::info!("friend {} from {}: now {}", fe.status, f.name, f.status);
            settled(s, &f);
        }
        Err(e) => tracing::warn!("friend event: {e}"),
    }
}
