//! Pairing and the device list. `docs/protocol.md` §2.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cued_proto::pair::{DesktopInfo, SealedAccount};
use cued_proto::{PairRequest, PairResponse};

use super::{Admin, ApiError, ApiResult, App, Phone};
use crate::state::{new_token, token_hash, UiEvent};

/// UI: mint a fresh pairing secret and QR code.
pub async fn new_link(_a: Admin, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let link = s.new_pair_link();
    let text = link.encode();
    Ok(Json(serde_json::json!({ "link": text, "svg": qr_svg(&text)?, "addresses": link.addresses, "funnel": link.funnel, "expiresInSec": 600 })))
}

pub async fn pending(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pending": s.pending_pair_count() }))
}

/// Phone: exchange the QR proof for a token.
pub async fn pair(State(s): State<App>, Json(req): Json<PairRequest>) -> ApiResult<(StatusCode, Json<PairResponse>)> {
    if req.v != cued_proto::PAIR_VERSION {
        return Err(ApiError::bad(format!("unsupported pair version {}", req.v)));
    }
    if !(req.device.len() == 16 && req.device.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(ApiError::bad("device must be 16 hex characters"));
    }
    if !req.pubkey.is_empty() && !(req.pubkey.len() == 64 && req.pubkey.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(ApiError::bad("pubkey must be 64 hex characters or empty"));
    }
    let name = req.name.trim().chars().take(40).collect::<String>();
    if name.is_empty() {
        return Err(ApiError::bad("name required"));
    }
    if !req.biokey.is_empty() && !cued_proto::bio::valid_biokey(&req.biokey) {
        return Err(ApiError::bad("biokey must be a SEC1 P-256 public key in hex"));
    }
    if !req.devhash.is_empty() && !(req.devhash.len() == 64 && req.devhash.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(ApiError::bad("devhash must be 64 hex characters"));
    }
    if !matches!(req.account.as_str(), "join" | "keep" | "separate") {
        return Err(ApiError::bad("account must be join, keep or separate"));
    }
    if s.db.device(&req.device)?.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "this phone is already paired; unlink it on the desktop first".into()));
    }
    let Some(secret) = s.take_pair_secret(&req) else {
        return Err(ApiError::forbidden("wrong or expired pairing secret; show a fresh QR on the desktop"));
    };
    let token = new_token();
    // A phone joining the account becomes a device of it; `keep` binds once the import lands.
    let pubkey = if req.account == "join" { s.identity().pubkey_hex() } else { req.pubkey.to_ascii_lowercase() };
    let dev = s.db.add_device(&req.device, &name, &pubkey, &token_hash(&token), &req.biokey.to_ascii_lowercase(), &req.devhash.to_ascii_lowercase())?;
    tracing::info!("paired {} ({}) account={} biokey={}", dev.name, dev.id, req.account, if dev.biokey.is_empty() { "none" } else { "yes" });
    let account = if req.account == "join" {
        if !dev.devhash.is_empty() {
            s.account.bind_device(&dev.devhash, &dev.name, "phone")?;
            s.emit(UiEvent::Account);
        }
        let bundle = serde_json::to_vec(&s.account.bundle())?;
        let secret_bytes = hex::decode(&secret).unwrap_or_default();
        Some(SealedAccount { sealed: cued_proto::seal::seal(&secret_bytes, "account", &bundle) })
    } else {
        None
    };
    s.emit(UiEvent::DevicePaired { id: dev.id.clone(), name: dev.name.clone() });
    if !dev.pubkey.is_empty() && dev.pubkey != s.identity().pubkey_hex() {
        // A paired phone's station is worth following by default; the person can unfollow in the UI.
        let _ = s.db.add_follow(&crate::db::Follow { pubkey: dev.pubkey.clone(), name: dev.name.clone(), relays: vec![], added_at: 0 });
        s.stations.poke();
    }
    let settings = s.settings();
    Ok((
        StatusCode::OK,
        Json(PairResponse {
            token,
            desktop: DesktopInfo {
                name: settings.name,
                pubkey: s.identity().pubkey_hex(),
                relay: s.relay_url(),
                funnel: s.funnel_url().map(|u| format!("{}/relay", u.replacen("https", "wss", 1))),
                cert: s.tls_fingerprint.clone(),
            },
            account,
        }),
    ))
}

pub async fn me(Phone(dev): Phone, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({ "device": dev, "desktop": { "name": s.settings().name, "pubkey": s.identity().pubkey_hex(), "relay": s.relay_url() } })))
}

pub async fn devices(_a: Admin, State(s): State<App>) -> ApiResult<Json<Vec<crate::db::DeviceStatus>>> {
    let mut out = Vec::new();
    for d in s.db.devices()? {
        out.push(s.db.device_status(&d)?);
    }
    Ok(Json(out))
}

pub async fn remove_device(_a: Admin, State(s): State<App>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if s.db.remove_device(&id)? {
        s.emit(UiEvent::DeviceRemoved { id });
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("no such device"))
    }
}

pub fn qr_svg(text: &str) -> ApiResult<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::L).map_err(|e| ApiError::bad(format!("too long for a QR code: {e}")))?;
    Ok(code.render::<svg::Color>().min_dimensions(256, 256).quiet_zone(true).dark_color(svg::Color("#0e0f13")).light_color(svg::Color("#ffffff")).build())
}
