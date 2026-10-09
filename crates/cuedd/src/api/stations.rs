//! Following, the live feed, hosting, identity, relay stats.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use cued_proto::{Link, StationLink};

use super::{Admin, AnyUser, ApiError, ApiResult, App};
use crate::db::Follow;

#[derive(serde::Deserialize)]
pub struct Reveal {
    #[serde(default)]
    pub reveal: bool,
}

pub async fn identity(_a: Admin, State(s): State<App>, Query(q): Query<Reveal>) -> Json<serde_json::Value> {
    let mut v = serde_json::json!({ "npub": s.identity.npub(), "pubkey": s.identity.pubkey_hex() });
    if q.reveal {
        v["nsec"] = serde_json::json!(s.identity.nsec());
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
    if f.pubkey == s.identity.pubkey_hex() {
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
    let entries: Vec<serde_json::Value> = s
        .stations
        .feed()
        .into_iter()
        .map(|e| {
            let pos = e.state.expected_position_ms(now);
            let mut v = serde_json::to_value(&e).unwrap_or_default();
            v["expectedPositionMs"] = serde_json::json!(pos);
            v["mine"] = serde_json::json!(e.pubkey == s.identity.pubkey_hex());
            v
        })
        .collect();
    Json(serde_json::json!({ "stations": entries, "upstream": s.stations.upstream_status(), "hosting": s.stations.hosting.load(std::sync::atomic::Ordering::Relaxed) }))
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
    let link = StationLink { pubkey: s.identity.pubkey_hex(), name: settings.station_name, relays: relays.into_iter().take(3).collect() };
    let text = link.encode();
    Ok(Json(serde_json::json!({ "link": text, "npub": s.identity.npub(), "svg": super::pair::qr_svg(&text)? })))
}

pub async fn relay_stats(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "url": s.relay_url(), "stats": s.relay.stats(), "upstream": s.stations.upstream_status() }))
}
