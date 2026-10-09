//! Pairing and the device list. `docs/protocol.md` §2.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cued_proto::pair::DesktopInfo;
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
    if !s.take_pair_secret(&req) {
        return Err(ApiError::forbidden("wrong or expired pairing secret; show a fresh QR on the desktop"));
    }
    if s.db.device(&req.device)?.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "this phone is already paired; unlink it on the desktop first".into()));
    }
    let token = new_token();
    let dev = s.db.add_device(&req.device, &name, &req.pubkey.to_ascii_lowercase(), &token_hash(&token))?;
    tracing::info!("paired {} ({})", dev.name, dev.id);
    s.emit(UiEvent::DevicePaired { id: dev.id.clone(), name: dev.name.clone() });
    if !dev.pubkey.is_empty() {
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
                pubkey: s.identity.pubkey_hex(),
                relay: s.relay_url(),
                funnel: s.funnel_url().map(|u| format!("{}/relay", u.replacen("https", "wss", 1))),
                cert: s.tls_fingerprint.clone(),
            },
        }),
    ))
}

pub async fn me(Phone(dev): Phone, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({ "device": dev, "desktop": { "name": s.settings().name, "pubkey": s.identity.pubkey_hex(), "relay": s.relay_url() } })))
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
