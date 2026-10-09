//! Following, the live feed, hosting, identity, relay stats.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use cued_proto::{Link, StationLink};

use super::{Admin, AnyUser, ApiError, ApiResult, App};
use crate::db::Follow;
use crate::state::UiEvent;

#[derive(serde::Deserialize)]
pub struct Reveal {
    #[serde(default)]
    pub reveal: bool,
}

pub async fn identity(_a: Admin, State(s): State<App>, Query(q): Query<Reveal>) -> Json<serde_json::Value> {
    let mut v = serde_json::json!({ "npub": s.identity().npub(), "pubkey": s.identity().pubkey_hex() });
    if q.reveal {
        v["nsec"] = serde_json::json!(s.identity().nsec());
    }
    Json(v)
}

pub async fn follows(_u: AnyUser, State(s): State<App>) -> ApiResult<Json<Vec<Follow>>> {
    Ok(Json(s.db.follows()?))
}

#[derive(serde::Deserialize)]
pub struct FollowBody {
    /// A `cued://station?…` link, an `npub…`, or 64 hex.
    pub text: String,
    #[serde(default)]
    pub name: String,
}

pub async fn follow(_a: Admin, State(s): State<App>, Json(b): Json<FollowBody>) -> ApiResult<Json<Follow>> {
    let t = b.text.trim();
    let f = match cued_proto::decode_link(t) {
        Ok(Link::Station(l)) => Follow { pubkey: l.pubkey, name: if b.name.is_empty() { l.name } else { b.name.clone() }, relays: l.relays, added_at: 0 },
        _ => match cued_proto::nip19::parse(t) {
            Some(("npub", hex)) | Some(("hex", hex)) => Follow { pubkey: hex, name: b.name.clone(), relays: vec![], added_at: 0 },
            Some(("nsec", _)) => return Err(ApiError::bad("that is a secret key; paste the npub or the cued://station link")),
            _ => return Err(ApiError::bad("paste a cued://station link or an npub")),
        },
    };
    if f.pubkey == s.identity().pubkey_hex() {
        return Err(ApiError::bad("that is this desktop's own station"));
    }
    s.db.add_follow(&f)?;
    // Relays named in the link are worth listening on too.
    if !f.relays.is_empty() {
        let _ = s.update_settings(|st| {
            for r in &f.relays {
                if !st.upstream_relays.contains(r) {
                    st.upstream_relays.push(r.clone());
                }
            }
        });
    }
    s.stations.poke();
    Ok(Json(s.db.follows()?.into_iter().find(|x| x.pubkey == f.pubkey).unwrap_or(f)))
}

pub async fn unfollow(_a: Admin, State(s): State<App>, Path(pubkey): Path<String>) -> ApiResult<StatusCode> {
    let pk = match cued_proto::nip19::parse(&pubkey) {
        Some((_, hex)) => hex,
        None => pubkey.to_ascii_lowercase(),
    };
    if s.db.remove_follow(&pk)? {
        s.stations.feed.lock().unwrap_or_else(|p| p.into_inner()).remove(&pk);
        s.stations.poke();
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("not following that station"))
    }
}

pub async fn feed(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    let now = crate::db::now_ms();
    let friends: std::collections::HashSet<String> = s.db.friends().unwrap_or_default().into_iter().filter(|f| f.status == "friends").map(|f| f.pubkey).collect();
    let listening = s.listen.listening();
    let entries: Vec<serde_json::Value> = s
        .stations
        .feed()
        .into_iter()
        .map(|e| {
            let pos = e.state.expected_position_ms(now);
            let mut v = serde_json::to_value(&e).unwrap_or_default();
            v["expectedPositionMs"] = serde_json::json!(pos);
            v["mine"] = serde_json::json!(e.pubkey == s.identity().pubkey_hex());
            v["friend"] = serde_json::json!(friends.contains(&e.pubkey));
            v["canListen"] = serde_json::json!(e.pubkey != s.identity().pubkey_hex() && e.state.now.as_ref().map(|n| n.stream.is_some() || n.link.is_some() || true).unwrap_or(false));
            v["listening"] = serde_json::json!(listening.as_deref() == Some(e.pubkey.as_str()));
            v
        })
        .collect();
    Json(serde_json::json!({ "stations": entries, "upstream": s.stations.upstream_status(), "hosting": s.stations.hosting.load(std::sync::atomic::Ordering::Relaxed), "relayEnabled": s.relay_enabled(), "listening": s.listen.status() }))
}

#[derive(serde::Deserialize)]
pub struct ListenBody {
    pub pubkey: String,
}

pub async fn listen_status(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    Json(s.listen.status())
}

pub async fn listen(_u: AnyUser, State(s): State<App>, Json(b): Json<ListenBody>) -> ApiResult<Json<serde_json::Value>> {
    let pk = match cued_proto::nip19::parse(&b.pubkey) {
        Some((_, hex)) => hex,
        None => return Err(ApiError::bad("bad pubkey")),
    };
    if pk == s.identity().pubkey_hex() {
        return Err(ApiError::bad("that is this desktop's own station"));
    }
    if !s.player.enabled() {
        return Err(ApiError::bad("audio engine is off"));
    }
    let name = s.stations.feed().into_iter().find(|e| e.pubkey == pk).map(|e| e.name).or_else(|| s.db.friend(&pk).ok().flatten().map(|f| f.name)).unwrap_or_default();
    if !s.db.follows()?.iter().any(|f| f.pubkey == pk) {
        s.db.add_follow(&crate::db::Follow { pubkey: pk.clone(), name: name.clone(), relays: vec![], added_at: 0 })?;
        s.stations.poke();
    }
    s.stations.hosting.store(false, std::sync::atomic::Ordering::Relaxed);
    s.listen.start(&pk, &name);
    s.emit(UiEvent::Listen);
    Ok(Json(s.listen.status()))
}

pub async fn stop_listening(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    s.listen.stop();
    s.player.stop();
    s.emit(UiEvent::Listen);
    Json(s.listen.status())
}

#[derive(serde::Deserialize)]
pub struct HostBody {
    pub on: bool,
    pub name: Option<String>,
}

pub async fn host(_a: Admin, State(s): State<App>, Json(b): Json<HostBody>) -> ApiResult<Json<serde_json::Value>> {
    if let Some(n) = b.name {
        s.update_settings(|st| st.station_name = n.trim().chars().take(40).collect())?;
    }
    s.stations.hosting.store(b.on, std::sync::atomic::Ordering::Relaxed);
    Ok(Json(serde_json::json!({ "hosting": b.on, "name": s.settings().station_name })))
}

pub async fn host_status(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "hosting": s.stations.hosting.load(std::sync::atomic::Ordering::Relaxed), "name": s.settings().station_name }))
}

/// The desktop's own follow link, as text and QR.
pub async fn my_link(_a: Admin, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let settings = s.settings();
    let mut relays = settings.upstream_relays.clone();
    relays.insert(0, s.relay_url());
    let link = StationLink { pubkey: s.identity().pubkey_hex(), name: settings.station_name, relays: relays.into_iter().take(3).collect() };
    let text = link.encode();
    Ok(Json(serde_json::json!({ "link": text, "npub": s.identity().npub(), "svg": super::pair::qr_svg(&text)? })))
}

pub async fn relay_stats(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "url": s.relay_url(), "enabled": s.relay_enabled(), "stats": s.relay.stats(), "upstream": s.stations.upstream_status() }))
}
