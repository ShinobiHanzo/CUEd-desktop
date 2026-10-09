//! HTTP surface: the phone API (`docs/protocol.md`), the UI's admin API, the
//! relay websocket, the companion proxy and the static UI.

pub mod companion;
pub mod library;
pub mod pair;
pub mod player;
pub mod stations;
pub mod sync;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Request, State};
use axum::http::{header, request::Parts, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use tower_http::cors::{Any, CorsLayer};

use crate::db::Device;
use crate::state::{AppState, Origin};

pub type App = Arc<AppState>;

/// Uniform error → JSON `{ "error": "…" }`.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self(StatusCode::FORBIDDEN, msg.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::warn!("internal: {e:#}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self(StatusCode::BAD_REQUEST, e.to_string())
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// A paired phone, from `Authorization: Bearer`.
pub struct Phone(pub Device);

#[axum::async_trait]
impl FromRequestParts<App> for Phone {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &App) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "bearer token required".into()))?;
        let dev = state.db.device_by_token_hash(&crate::state::token_hash(&token))?.ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "unknown token".into()))?;
        state.db.touch_device(&dev.id, false, false)?;
        Ok(Phone(dev))
    }
}

/// The person at the desktop: a request on the LAN listener from a loopback address.
pub struct Admin;

#[axum::async_trait]
impl FromRequestParts<App> for Admin {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, _state: &App) -> Result<Self, Self::Rejection> {
        let origin = parts.extensions.get::<Origin>().copied().unwrap_or(Origin::External);
        let addr = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        match (origin, addr) {
            (Origin::Lan, Some(ip)) if crate::state::is_loopback(ip) => Ok(Admin),
            _ => Err(ApiError::forbidden("this needs the desktop UI (localhost)")),
        }
    }
}

/// Either the desktop's own UI or a paired phone.
pub struct AnyUser;

#[axum::async_trait]
impl FromRequestParts<App> for AnyUser {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &App) -> Result<Self, Self::Rejection> {
        if Admin::from_request_parts(parts, state).await.is_ok() {
            return Ok(AnyUser);
        }
        Phone::from_request_parts(parts, state).await.map(|_| AnyUser)
    }
}

fn bearer(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(|s| s.trim().to_string())
        .or_else(|| {
            // Phones streaming through a media player cannot always set headers: allow `?token=`.
            parts.uri.query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=").map(str::to_string)))
        })
}

async fn tag_origin(Extension(origin): Extension<Origin>, mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(origin);
    next.run(req).await
}

pub fn router(state: App, origin: Origin) -> Router {
    let api = Router::new()
        // pairing & devices
        .route("/pair", post(pair::pair))
        .route("/pair/new", post(pair::new_link))
        .route("/pair/pending", get(pair::pending))
        .route("/devices", get(pair::devices))
        .route("/devices/:id", delete(pair::remove_device))
        .route("/me", get(pair::me))
        .route("/status", get(status))
        .route("/settings", get(settings).put(put_settings))
        // sync / backup
        .route("/sync/manifest", post(sync::manifest))
        .route("/sync/track/:sha", put(sync::upload).head(sync::have))
        .route("/sync/status", get(sync::status))
        .route("/backup/snapshot", post(sync::snapshot))
        .route("/backup/snapshots", get(sync::snapshots))
        .route("/backup/blob/:name", put(sync::put_blob).get(sync::get_blob))
        .route("/backup/blobs", get(sync::blobs))
        // library
        .route("/library", get(library::list))
        .route("/library/import", post(library::import))
        .route("/library/analyse/:sha", post(library::analyse))
        .route("/track/:sha", get(library::stream))
        .route("/track/:sha/meta", get(library::meta))
        .route("/share/:sha", post(library::share))
        .route("/receive", post(library::receive))
        .route("/qr", get(library::qr))
        // player
        .route("/player", get(player::status))
        .route("/player/load", post(player::load))
        .route("/player/play", post(player::play))
        .route("/player/pause", post(player::pause))
        .route("/player/stop", post(player::stop))
        .route("/player/seek", post(player::seek))
        .route("/player/volume", post(player::volume))
        .route("/player/queue", get(player::queue).post(player::enqueue).delete(player::clear_queue))
        .route("/player/next", post(player::next))
        .route("/player/spectrum", get(player::spectrum))
        // stations
        .route("/identity", get(stations::identity))
        .route("/stations/follows", get(stations::follows).post(stations::follow))
        .route("/stations/follows/:pubkey", delete(stations::unfollow))
        .route("/stations/feed", get(stations::feed))
        .route("/stations/host", get(stations::host_status).post(stations::host))
        .route("/stations/link", get(stations::my_link))
        .route("/relay/stats", get(stations::relay_stats))
        .route("/funnel", get(funnel))
        .layer(DefaultBodyLimit::max(4 << 30));

    Router::new()
        .nest("/api", api)
        .route("/share/:sha/:cap", get(library::share_download))
        .route("/relay", get(crate::relay_ws::handler))
        .route("/ws/events", get(player::events_ws))
        // companion API at the root, exactly where the phone's Companion backend expects it
        .route("/health", get(companion::health))
        .route("/jobs", post(companion::create_job).get(companion::list_jobs))
        .route("/jobs/:id", get(companion::job).delete(companion::delete_job))
        .route("/files/:name", get(companion::file))
        .fallback(crate::ui::serve)
        .layer(middleware::from_fn(tag_origin))
        .layer(Extension(origin))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}

async fn status(State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let (tracks, bytes) = s.db.track_count()?;
    let devices = s.db.devices()?;
    let relay = s.relay.stats();
    let settings = s.settings();
    let companion = s.companion.lock().unwrap_or_else(|p| p.into_inner()).clone();
    Ok(Json(serde_json::json!({
        "name": settings.name,
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSec": s.started.elapsed().as_secs(),
        "dataDir": s.data_dir,
        "lan": s.lan_urls(),
        "relay": s.relay_url(),
        "funnel": { "url": s.funnel_url(), "cert": s.tls_fingerprint, "status": s.funnel_status.snapshot() },
        "library": { "tracks": tracks, "bytes": bytes },
        "devices": devices.len(),
        "relayStats": relay,
        "identity": { "npub": s.identity.npub(), "pubkey": s.identity.pubkey_hex() },
        "companion": companion,
        "audio": s.player.describe(),
        "pendingPairs": s.pending_pair_count(),
    })))
}

async fn settings(_a: Admin, State(s): State<App>) -> Json<crate::config::Settings> {
    Json(s.settings())
}

async fn put_settings(_a: Admin, State(s): State<App>, Json(new): Json<serde_json::Value>) -> ApiResult<Json<crate::config::Settings>> {
    let updated = s.update_settings(|cur| {
        let mut v = serde_json::to_value(&*cur).unwrap_or_default();
        if let (Some(a), Some(b)) = (v.as_object_mut(), new.as_object()) {
            for (k, val) in b {
                a.insert(k.clone(), val.clone());
            }
        }
        if let Ok(parsed) = serde_json::from_value::<crate::config::Settings>(v) {
            *cur = parsed;
        }
        cur.upstream_relays.retain(|r| r.starts_with("wss://") || r.starts_with("ws://"));
        cur.crossfade_seconds = cur.crossfade_seconds.clamp(0.0, 20.0);
        cur.crossfade_curve = cur.crossfade_curve.min(2);
    })?;
    s.stations.poke();
    Ok(Json(updated))
}

async fn funnel(State(s): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "url": s.funnel_url(), "cert": s.tls_fingerprint, "status": s.funnel_status.snapshot() }))
}

/// Streams a file with HTTP range support.
pub async fn range_file(path: &std::path::Path, mime: &str, range: Option<&HeaderValue>, download_name: Option<&str>) -> ApiResult<Response> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await.map_err(|_| ApiError::not_found("file is gone"))?;
    let total = file.metadata().await?.len();
    let (start, end) = match range.and_then(|r| r.to_str().ok()).and_then(|r| parse_range(r, total)) {
        Some(r) => r,
        None if range.is_some() => {
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{total}"))]).into_response());
        }
        None => (0, total.saturating_sub(1)),
    };
    let len = if total == 0 { 0 } else { end - start + 1 };
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let stream = tokio_util::io::ReaderStream::with_capacity(file.take(len), 1 << 16);
    let mut resp = Response::builder()
        .status(if range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len);
    if range.is_some() {
        resp = resp.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"));
    }
    if let Some(n) = download_name {
        resp = resp.header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", n.replace('"', "'")));
    }
    Ok(resp.body(Body::from_stream(stream)).unwrap())
}

fn parse_range(h: &str, total: u64) -> Option<(u64, u64)> {
    let spec = h.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (total.saturating_sub(n), total.saturating_sub(1))
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() { total.saturating_sub(1) } else { b.parse::<u64>().ok()?.min(total.saturating_sub(1)) };
        (s, e)
    };
    if total == 0 || start > end || start >= total {
        return None;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    #[test]
    fn ranges() {
        use super::parse_range;
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=0-5000", 1000), Some((0, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
    }
}
