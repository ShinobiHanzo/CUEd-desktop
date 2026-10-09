//! Friends (`docs/protocol.md` §9): list, requests, accept, remove, my link.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cued_proto::{FriendLink, Link};

use super::{Admin, AnyUser, ApiError, ApiResult, App};
use crate::db::Friend;

pub async fn list(_u: AnyUser, State(s): State<App>) -> ApiResult<Json<Vec<Friend>>> {
    Ok(Json(s.db.friends()?))
}

pub async fn requests(_u: AnyUser, State(s): State<App>) -> ApiResult<Json<Vec<Friend>>> {
    Ok(Json(s.db.friends()?.into_iter().filter(|f| f.status == "pending_in").collect()))
}

pub async fn my_link(_u: AnyUser, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let mut relays = s.settings().upstream_relays;
    if s.relay_enabled() {
        relays.insert(0, s.relay_url());
    }
    let link = FriendLink { pubkey: s.identity().pubkey_hex(), name: s.account.chain().name(), relays: relays.into_iter().take(3).collect() };
    let text = link.encode();
    Ok(Json(serde_json::json!({ "link": text, "npub": s.identity().npub(), "svg": super::pair::qr_svg(&text)? })))
}

#[derive(serde::Deserialize)]
pub struct AddBody {
    /// A `cued://friend?…` link, a `cued://share?…&fr=` link, an `npub…` or 64 hex.
    pub text: String,
    #[serde(default)]
    pub name: String,
}

pub async fn add(_u: AnyUser, State(s): State<App>, Json(b): Json<AddBody>) -> ApiResult<Json<Friend>> {
    let t = b.text.trim();
    let (pubkey, name, relays) = match cued_proto::decode_link(t) {
        Ok(Link::Friend(l)) => (l.pubkey, if b.name.is_empty() { l.name } else { b.name.clone() }, l.relays),
        Ok(Link::Share(p)) if p.friend_pubkey.is_some() => (p.friend_pubkey.unwrap_or_default(), p.friend_name.unwrap_or_default(), vec![]),
        _ => match cued_proto::nip19::parse(t) {
            Some(("npub", hex)) | Some(("hex", hex)) => (hex, b.name.clone(), vec![]),
            Some(("nsec", _)) => return Err(ApiError::bad("that is a secret key; paste the npub or the cued://friend link")),
            _ => return Err(ApiError::bad("paste a cued://friend link, a share link with a friend request, or an npub")),
        },
    };
    if pubkey == s.identity().pubkey_hex() {
        return Err(ApiError::bad("that is your own account"));
    }
    let f = crate::friends::request(&s, &pubkey, &name, &relays)?;
    Ok(Json(f))
}

pub async fn accept(_u: AnyUser, State(s): State<App>, Path(pubkey): Path<String>) -> ApiResult<Json<Friend>> {
    let pk = key(&pubkey)?;
    let f = crate::friends::accept(&s, &pk)?.ok_or_else(|| ApiError::not_found("no request from that key"))?;
    Ok(Json(f))
}

pub async fn remove(_u: AnyUser, State(s): State<App>, Path(pubkey): Path<String>) -> ApiResult<StatusCode> {
    let pk = key(&pubkey)?;
    if crate::friends::remove(&s, &pk)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("not a friend"))
    }
}

fn key(text: &str) -> ApiResult<String> {
    match cued_proto::nip19::parse(text) {
        Some(("npub", hex)) | Some(("hex", hex)) => Ok(hex),
        _ => Err(ApiError::bad("bad pubkey")),
    }
}

#[allow(dead_code)]
async fn _admin_only(_a: Admin) {}
