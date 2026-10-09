//! The account and its chain (`docs/protocol.md` §8) and the shared settings (§11).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use cued_proto::account::Chain;

use super::{Admin, AnyUser, ApiError, ApiResult, App, Phone};
use crate::state::UiEvent;

pub async fn summary(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    Json(s.account.summary())
}

pub async fn chain(_u: AnyUser, State(s): State<App>) -> Json<Chain> {
    Json(s.account.chain())
}

#[derive(serde::Deserialize)]
pub struct ChainBody {
    pub blocks: Vec<cued_proto::account::Block>,
}

/// Another device of the account sends its copy; the longer valid chain wins.
pub async fn merge_chain(_u: AnyUser, State(s): State<App>, Json(b): Json<ChainBody>) -> ApiResult<Json<serde_json::Value>> {
    let other = Chain { blocks: b.blocks };
    let changed = s.account.merge(&other).map_err(|e| ApiError::bad(e.to_string()))?;
    if changed {
        s.emit(UiEvent::Account);
        s.emit(UiEvent::Shared);
        s.stations.poke();
    }
    Ok(Json(serde_json::json!({ "changed": changed, "height": s.account.chain().height(), "tip": s.account.chain().tip_hash() })))
}

#[derive(serde::Deserialize)]
pub struct ImportBody {
    pub sealed: String,
}

/// `account: "keep"` at pairing: the phone hands over its account, sealed with the pairing secret.
pub async fn import(Phone(dev): Phone, State(s): State<App>, Json(b): Json<ImportBody>) -> ApiResult<Json<serde_json::Value>> {
    let secret = s.recent_pair_secret(&dev.id).ok_or_else(|| ApiError::forbidden("import is only possible within 10 minutes of pairing"))?;
    let bytes = cued_proto::seal::open(&hex::decode(secret).unwrap_or_default(), "account", &b.sealed).map_err(|e| ApiError::bad(e.to_string()))?;
    let bundle: cued_proto::AccountBundle = serde_json::from_slice(&bytes).map_err(|e| ApiError::bad(format!("bundle: {e}")))?;
    let my_name = s.settings().name;
    s.account.adopt(bundle, &my_name).map_err(|e| ApiError::bad(e.to_string()))?;
    if !dev.devhash.is_empty() {
        let _ = s.account.bind_device(&dev.devhash, &dev.name, "phone");
    }
    // The phone is now a device of the same account: its pubkey in the device row follows.
    let _ = s.db.set_device_pubkey(&dev.id, &s.identity().pubkey_hex());
    tracing::info!("account replaced by {}'s ({})", dev.name, s.identity().npub());
    s.emit(UiEvent::Account);
    s.emit(UiEvent::Shared);
    s.stations.poke();
    Ok(Json(s.account.summary()))
}

#[derive(serde::Deserialize)]
pub struct Reveal {
    #[serde(default)]
    pub reveal: bool,
}

pub async fn export(_a: Admin, State(s): State<App>, Query(q): Query<Reveal>) -> ApiResult<Json<serde_json::Value>> {
    if !q.reveal {
        return Err(ApiError::bad("add ?reveal=true to export the private key"));
    }
    let id = s.identity();
    Ok(Json(serde_json::json!({ "nsec": id.nsec(), "npub": id.npub(), "chain": s.account.chain() })))
}

#[derive(serde::Deserialize)]
pub struct NameBody {
    pub name: String,
}

pub async fn profile(_u: AnyUser, State(s): State<App>, Json(b): Json<NameBody>) -> ApiResult<Json<serde_json::Value>> {
    if b.name.trim().is_empty() {
        return Err(ApiError::bad("name required"));
    }
    s.account.set_name(&b.name)?;
    s.emit(UiEvent::Account);
    Ok(Json(s.account.summary()))
}

#[derive(serde::Deserialize)]
pub struct DevhashBody {
    pub devhash: String,
}

pub async fn unbind(_u: AnyUser, State(s): State<App>, Json(b): Json<DevhashBody>) -> ApiResult<Json<serde_json::Value>> {
    s.account.unbind_device(&b.devhash.to_ascii_lowercase()).map_err(|e| ApiError::bad(e.to_string()))?;
    s.emit(UiEvent::Account);
    Ok(Json(s.account.summary()))
}

pub async fn renew(_a: Admin, State(s): State<App>, Json(b): Json<NameBody>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let my_name = s.settings().name;
    s.account.renew(b.name.trim(), &my_name)?;
    s.stations.hosting.store(false, std::sync::atomic::Ordering::Relaxed);
    s.emit(UiEvent::Account);
    s.emit(UiEvent::Shared);
    s.stations.poke();
    Ok((StatusCode::CREATED, Json(s.account.summary())))
}

pub async fn shared(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
    Json(s.account.shared())
}

pub async fn put_shared(_u: AnyUser, State(s): State<App>, Json(patch): Json<serde_json::Value>) -> ApiResult<Json<serde_json::Value>> {
    let before = s.relay_enabled();
    let v = s.account.update_shared(&patch).map_err(|e| ApiError::bad(e.to_string()))?;
    s.emit(UiEvent::Shared);
    if before != s.relay_enabled() {
        tracing::info!("desktop relay {}", if s.relay_enabled() { "on" } else { "off" });
        s.stations.poke();
    }
    Ok(Json(v))
}
