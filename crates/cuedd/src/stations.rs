//! Stations on the desktop: the live feed of followed stations, mirroring
//! between the embedded relay and the upstream relays, and hosting a station
//! from the desktop player.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cued_proto::nostr::{self, Event, RelayMessage};
use cued_proto::{station, StationState, StationTrack};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::state::{AppState, UiEvent};

#[derive(Debug, Clone, Serialize)]
pub struct FeedEntry {
    pub pubkey: String,
    pub npub: String,
    pub name: String,
    pub state: StationState,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    /// Where it came from: `local` (embedded relay) or an upstream URL.
    pub via: String,
    pub following: bool,
}

#[derive(Default)]
pub struct StationsShared {
    notify: Notify,
    pub feed: Mutex<HashMap<String, FeedEntry>>,
    pub hosting: AtomicBool,
    pub upstream: Mutex<HashMap<String, bool>>,
}

impl StationsShared {
    /// Follows or settings changed: rebuild subscriptions.
    pub fn poke(&self) {
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
    pub fn feed(&self) -> Vec<FeedEntry> {
        let mut v: Vec<FeedEntry> = self.feed.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        v
    }
    pub fn upstream_status(&self) -> HashMap<String, bool> {
        self.upstream.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

fn feed_update(state: &AppState, ev: &Event, via: &str) {
    if ev.kind != station::KIND || ev.tag("d") != Some(station::D_TAG) {
        return;
    }
    let Ok(st) = StationState::decode(&ev.content) else { return };
    let following: HashSet<String> = state.db.follows().unwrap_or_default().into_iter().map(|f| f.pubkey).collect();
    let name = if st.name.is_empty() { state.db.follows().unwrap_or_default().into_iter().find(|f| f.pubkey == ev.pubkey).map(|f| f.name).unwrap_or_default() } else { st.name.clone() };
    let mut feed = state.stations.feed.lock().unwrap_or_else(|p| p.into_inner());
    let newer = feed.get(&ev.pubkey).map(|e| ev.created_at > e.created_at || (ev.created_at == e.created_at && st.seq >= e.state.seq)).unwrap_or(true);
    if newer {
        feed.insert(ev.pubkey.clone(), FeedEntry { pubkey: ev.pubkey.clone(), npub: cued_proto::nip19::npub(&ev.pubkey), name, state: st, created_at: ev.created_at, via: via.into(), following: following.contains(&ev.pubkey) });
        drop(feed);
        state.emit(UiEvent::Station { pubkey: ev.pubkey.clone() });
    }
}

pub fn spawn(state: Arc<AppState>, shutdown: CancellationToken) {
    // Seed the feed with what the embedded relay already holds.
    for f in state.db.follows().unwrap_or_default() {
        if let Some(ev) = state.relay.latest_by_author(station::KIND, &f.pubkey, Some(station::D_TAG)) {
            feed_update(&state, &ev, "local");
        }
    }
    if let Some(ev) = state.relay.latest_by_author(station::KIND, &state.identity().pubkey_hex(), Some(station::D_TAG)) {
        feed_update(&state, &ev, "local");
    }

    let (out_tx, out_rx) = mpsc::unbounded_channel::<Event>();

    // Embedded relay → feed + mirror out.
    {
        let state = state.clone();
        let sd = shutdown.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let mut rx = state.relay.subscribe_all();
            loop {
                tokio::select! {
                    _ = sd.cancelled() => break,
                    ev = rx.recv() => match ev {
                        Ok(ev) => {
                            feed_update(&state, &ev, "local");
                            crate::friends::on_event(&state, &ev);
                            if state.relay_enabled() && state.settings().mirror_out && is_ours(&state, &ev.pubkey) {
                                let _ = out_tx.send(ev);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
            }
        });
    }

    // Upstream relays.
    tokio::spawn(upstream_loop(state.clone(), out_rx, shutdown.clone()));
    // Host.
    tokio::spawn(host_loop(state.clone(), shutdown.clone()));
    crate::listen::spawn(state.clone(), shutdown.clone());
    crate::player::spawn_ticker(state, shutdown);
}

/// Events worth re-publishing upstream: the desktop's own and its paired phones'.
fn is_ours(state: &AppState, pubkey: &str) -> bool {
    pubkey == state.identity().pubkey_hex() || state.db.devices().unwrap_or_default().iter().any(|d| d.pubkey == pubkey)
}

async fn upstream_loop(state: Arc<AppState>, mut out_rx: mpsc::UnboundedReceiver<Event>, shutdown: CancellationToken) {
    let mut children: HashMap<String, (CancellationToken, mpsc::UnboundedSender<Event>)> = HashMap::new();
    let mut backlog: Vec<Event> = Vec::new();
    loop {
        // (Re)build the set of relay tasks from the settings.
        // The desktop relay is a switch shared by every device of the account; off means no upstream traffic either.
        let wanted: Vec<String> = if state.relay_enabled() && (state.settings().mirror_in || state.settings().mirror_out) { state.settings().upstream_relays.clone() } else { vec![] };
        children.retain(|url, (tok, _)| {
            if wanted.contains(url) {
                true
            } else {
                tok.cancel();
                state.stations.upstream.lock().unwrap_or_else(|p| p.into_inner()).remove(url);
                false
            }
        });
        for url in &wanted {
            if !children.contains_key(url) {
                let tok = shutdown.child_token();
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::spawn(relay_task(state.clone(), url.clone(), rx, tok.clone()));
                children.insert(url.clone(), (tok, tx));
            }
        }
        for ev in backlog.drain(..) {
            for (_, tx) in children.values() {
                let _ = tx.send(ev.clone());
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = state.stations.notify.notified() => {
                // Settings or follows changed: tell every relay task to resubscribe by restarting it.
                for (_, (tok, _)) in children.drain() { tok.cancel(); }
            }
            ev = out_rx.recv() => {
                if let Some(ev) = ev {
                    if children.is_empty() { backlog.push(ev); } else { for (_, tx) in children.values() { let _ = tx.send(ev.clone()); } }
                } else { break; }
            }
        }
    }
}

async fn relay_task(state: Arc<AppState>, url: String, mut out: mpsc::UnboundedReceiver<Event>, shutdown: CancellationToken) {
    let mut attempt = 0u32;
    while !shutdown.is_cancelled() {
        let set = |ok: bool| {
            state.stations.upstream.lock().unwrap_or_else(|p| p.into_inner()).insert(url.clone(), ok);
        };
        match tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&url)).await {
            Ok(Ok((ws, _))) => {
                attempt = 0;
                set(true);
                info!("relay {url}: connected");
                let (mut tx, mut rx) = ws.split();
                // Subscribe to everyone we follow, plus the desktop's own station and the phones' (to see our mirror land).
                let mut authors: Vec<String> = state.db.follows().unwrap_or_default().into_iter().map(|f| f.pubkey).collect();
                authors.push(state.identity().pubkey_hex());
                for d in state.db.devices().unwrap_or_default() {
                    if !d.pubkey.is_empty() {
                        authors.push(d.pubkey);
                    }
                }
                authors.sort();
                authors.dedup();
                if state.settings().mirror_in && !authors.is_empty() {
                    let me = state.identity().pubkey_hex();
                    let _ = tx.send(tokio_tungstenite::tungstenite::Message::Text(nostr::req_message("cued-stations", &[nostr::station_filter(&authors, None), cued_proto::friend::filter(&me, None)]))).await;
                }
                // Push the newest state of ours so a relay that was down catches up.
                if state.settings().mirror_out {
                    if let Some(ev) = state.relay.latest_by_author(station::KIND, &state.identity().pubkey_hex(), Some(station::D_TAG)) {
                        let _ = tx.send(tokio_tungstenite::tungstenite::Message::Text(nostr::event_message(&ev))).await;
                    }
                }
                let mut ping = tokio::time::interval(Duration::from_secs(25));
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => { let _ = tx.close().await; set(false); return; }
                        _ = ping.tick() => { if tx.send(tokio_tungstenite::tungstenite::Message::Ping(vec![])).await.is_err() { break; } }
                        ev = out.recv() => match ev {
                            Some(ev) => { if tx.send(tokio_tungstenite::tungstenite::Message::Text(nostr::event_message(&ev))).await.is_err() { break; } }
                            None => { set(false); return; }
                        },
                        msg = rx.next() => match msg {
                            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                match nostr::parse_relay_message(&text) {
                                    RelayMessage::Event { event, .. } => {
                                        if event.verify() && (authors.contains(&event.pubkey) || event.kind == cued_proto::friend::FRIEND_KIND) {
                                            if state.settings().mirror_in { let _ = state.relay.publish(&event); }
                                            feed_update(&state, &event, &url);
                                            crate::friends::on_event(&state, &event);
                                        }
                                    }
                                    RelayMessage::Ok { accepted: false, event_id, message } => warn!("relay {url} rejected {}: {message}", &event_id[..8.min(event_id.len())]),
                                    RelayMessage::Notice(n) => debug!("relay {url}: {n}"),
                                    _ => {}
                                }
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => { warn!("relay {url}: {e}"); break; }
                            None => break,
                        }
                    }
                }
                set(false);
            }
            Ok(Err(e)) => {
                set(false);
                warn!("relay {url}: {e}");
            }
            Err(_) => {
                set(false);
                warn!("relay {url}: connect timeout");
            }
        }
        let wait = Duration::from_secs(1u64 << attempt.min(6));
        attempt += 1;
        tokio::select! { _ = shutdown.cancelled() => return, _ = tokio::time::sleep(wait) => {} }
    }
}

/// A station entry for `q`; with friend streaming on, capability URLs for the audio, cover and lyrics ride along.
fn to_station_track(state: &AppState, q: &crate::player::QueueItem) -> StationTrack {
    let mut t = StationTrack { title: q.title.clone(), artist: q.artist.clone(), album: q.album.clone(), duration_ms: q.duration_ms, link: q.link.clone().filter(|l| l.starts_with("http")), cover: None, key: q.sha256.clone(), stream: None, lyrics: None };
    if state.account.friend_streaming() && !q.sha256.is_empty() {
        let cap = state.new_share_cap_for(&q.sha256, Duration::from_secs(2 * 3600));
        let base = state.public_base();
        t.stream = Some(format!("{base}/share/{}/{cap}", q.sha256));
        t.cover = Some(format!("{base}/share/{}/{cap}/cover", q.sha256));
        t.lyrics = Some(format!("{base}/share/{}/{cap}/lyrics", q.sha256));
    }
    t
}

/// Publishes the desktop's own station state while hosting: on every player change and every 30 s.
async fn host_loop(state: Arc<AppState>, shutdown: CancellationToken) {
    let mut iv = tokio::time::interval(Duration::from_millis(500));
    let mut last_gen = u64::MAX;
    let mut last_pub = tokio::time::Instant::now() - Duration::from_secs(60);
    let mut seq = nostr::now_secs();
    let mut was_hosting = false;
    loop {
        tokio::select! { _ = shutdown.cancelled() => break, _ = iv.tick() => {} }
        let hosting = state.stations.hosting.load(Ordering::Relaxed);
        if !hosting {
            if was_hosting {
                seq += 1;
                let st = StationState { name: state.settings().station_name, status: station::STATUS_OFF.into(), seq, ..Default::default() };
                let ev = state.identity().sign_now(station::KIND, station::tags(), st.encode());
                let _ = state.relay.publish(&ev);
                was_hosting = false;
            }
            continue;
        }
        was_hosting = true;
        let gen = state.player.generation();
        if gen == last_gen && last_pub.elapsed() < Duration::from_secs(30) {
            continue;
        }
        last_gen = gen;
        last_pub = tokio::time::Instant::now();
        let (current, queue, pos, playing) = state.player.snapshot();
        let now_ms = crate::db::now_ms();
        let pos_ms = (pos * 1000.0) as i64;
        seq += 1;
        let st = StationState {
            v: 1,
            name: state.settings().station_name,
            status: if current.is_none() { station::STATUS_PAUSED.into() } else if playing { station::STATUS_ON_AIR.into() } else { station::STATUS_PAUSED.into() },
            now: current.as_ref().map(|c| to_station_track(&state, c)),
            started_at: now_ms - pos_ms,
            position_ms: pos_ms,
            next: queue.iter().take(3).map(|q| to_station_track(&state, q)).collect(),
            seq,
        };
        let ev = state.identity().sign_now(station::KIND, station::tags(), st.encode());
        match state.relay.publish(&ev) {
            cued_relay::Accept::Rejected(r) => warn!("own station event rejected: {r}"),
            _ => debug!("station published seq={seq} {}", st.status),
        }
    }
}
