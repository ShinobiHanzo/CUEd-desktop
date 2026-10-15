//! HTTP surface: the phone API (`docs/protocol.md`), the UI's admin API, the
//! relay websocket, the companion proxy and the static UI.

pub mod account;
pub mod companion;
pub mod friends;
pub mod library;
pub mod pair;
pub mod party;
pub mod player;
pub mod stations;
pub mod sync;
pub mod update;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Request, State};
use axum::http::{header, request::Parts, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};

use crate::db::Device;
use crate::state::{AppState, Origin, Tier};

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
        let tier = parts.extensions.get::<Tier>().copied().unwrap_or(Tier::Remote);
        if tier == Tier::Remote && !matches!(parts.method, Method::GET | Method::HEAD) {
            return Err(ApiError::forbidden("writes only on the LAN"));
        }
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
        match parts.extensions.get::<Tier>().copied() {
            Some(Tier::Admin) => Ok(Admin),
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
    bearer_of(&parts.headers, parts.uri.query())
}

fn bearer_of(headers: &axum::http::HeaderMap, query: Option<&str>) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(|s| s.trim().to_string())
        .or_else(|| {
            // Phones streaming through a media player cannot always set headers: allow `?token=`.
            query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=").map(str::to_string)))
        })
}

/// Read-only routes a phone may use away from home (`docs/protocol.md` §7).
fn remote_readable(path: &str) -> bool {
    const EXACT: [&str; 14] = [
        "/api/me", "/api/library", "/api/sync/status", "/api/backup/snapshots", "/api/backup/blobs", "/api/stations/feed", "/api/stations/follows",
        "/api/stations/listen", "/api/friends", "/api/friends/requests", "/api/friends/link", "/api/account", "/api/account/chain", "/api/settings/shared",
    ];
    const PREFIX: [&str; 2] = ["/api/track/", "/api/backup/blob/"];
    EXACT.contains(&path) || PREFIX.iter().any(|p| path.starts_with(p)) || path == "/api/auth/challenge"
}

/// Uploads (a track, a backup blob) may be large; every other route keeps axum's 2 MB.
const UPLOAD_LIMIT: usize = 4 << 30;

/// The host part of `Host` (or `:authority`), without the port.
fn host_name(headers: &axum::http::HeaderMap) -> Option<String> {
    let h = headers.get(header::HOST)?.to_str().ok()?.trim();
    Some(strip_port(h).to_ascii_lowercase())
}

fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority)
}

/// `true` when the request was addressed to this machine by a loopback name or address.
pub fn host_is_loopback(headers: &axum::http::HeaderMap) -> bool {
    match host_name(headers) {
        Some(h) => h == "localhost" || h.ends_with(".localhost") || h.parse::<std::net::IpAddr>().map(crate::state::is_loopback).unwrap_or(false),
        None => false,
    }
}

/// `true` for a non-browser client (no `Origin`, no `Sec-Fetch-Site`) or a browser request
/// from the UI's own origin; `false` for a page on another site poking at localhost.
pub fn same_site_or_native(headers: &axum::http::HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if !matches!(site, "same-origin" | "none") {
            return false;
        }
    }
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some("null") => false,
        Some(o) => {
            let authority = o.split("://").nth(1).unwrap_or("").trim_end_matches('/');
            let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
            authority.eq_ignore_ascii_case(host)
        }
    }
}

/// The three tiers: decides who a request is before any handler runs.
async fn policy(State(s): State<App>, Extension(origin): Extension<Origin>, ConnectInfo(addr): ConnectInfo<SocketAddr>, mut req: Request, next: Next) -> Response {
    let ip = addr.ip();
    let tier = match origin {
        // The desktop UI: a loopback peer that also asked for a loopback name (a page a
        // rebound DNS name points at 127.0.0.1 is not it), and no cross-site browser write.
        Origin::Lan if crate::state::is_loopback(ip) && host_is_loopback(req.headers()) => {
            if !same_site_or_native(req.headers()) && !matches!(*req.method(), Method::GET | Method::HEAD) {
                return ApiError::forbidden("cross-site request to the desktop UI refused").into_response();
            }
            Tier::Admin
        }
        Origin::Lan if crate::state::is_lan(ip) => Tier::Lan,
        _ => Tier::Remote,
    };
    req.extensions_mut().insert(origin);
    req.extensions_mut().insert(tier);
    if tier != Tier::Remote {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let read_only = matches!(*req.method(), Method::GET | Method::HEAD);
    // Capability links (`/share/<sha>/<cap>…`) are how friends stream: the capability is the credential.
    if read_only && path.starts_with("/share/") {
        return next.run(req).await;
    }
    if path == "/relay" {
        if !s.relay_enabled() {
            return ApiError::forbidden("the desktop relay is off").into_response();
        }
    } else if !(read_only && remote_readable(&path)) {
        return ApiError::forbidden("writes only on the LAN").into_response();
    }
    let Some(token) = bearer_of(req.headers(), req.uri().query()) else {
        return ApiError(StatusCode::UNAUTHORIZED, "bearer token required".into()).into_response();
    };
    let dev = match s.db.device_by_token_hash(&crate::state::token_hash(&token)) {
        Ok(Some(d)) => d,
        _ => return ApiError(StatusCode::UNAUTHORIZED, "unknown token".into()).into_response(),
    };
    if path == "/api/auth/challenge" {
        return next.run(req).await;
    }
    let assertion = req
        .headers()
        .get("x-cued-assertion")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("assertion=").map(str::to_string))));
    match assertion {
        Some(a) if s.check_assertion(&dev, &a) => next.run(req).await,
        _ if dev.biokey.is_empty() => ApiError::forbidden("this phone registered no biometric key; pair again on the LAN").into_response(),
        _ => ApiError::forbidden("biometric assertion required: GET /api/auth/challenge, sign it, send X-Cued-Assertion").into_response(),
    }
}

/// `GET /api/auth/challenge`: a nonce for the phone's biometric key to sign.
async fn challenge(Phone(dev): Phone, State(s): State<App>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "challenge": s.new_challenge(&dev.id), "expiresInSec": 120, "hasBiokey": !dev.biokey.is_empty() }))
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
        .route("/settings/shared", get(account::shared).put(account::put_shared))
        .route("/auth/challenge", get(challenge))
        // account
        .route("/account", get(account::summary))
        .route("/account/chain", get(account::chain).post(account::merge_chain))
        .route("/account/import", post(account::import))
        .route("/account/export", get(account::export))
        .route("/account/profile", post(account::profile))
        .route("/account/unbind", post(account::unbind))
        .route("/account/new", post(account::renew))
        // friends
        .route("/friends", get(friends::list).post(friends::add))
        .route("/friends/requests", get(friends::requests))
        .route("/friends/link", get(friends::my_link))
        .route("/friends/:pubkey", delete(friends::remove))
        .route("/friends/:pubkey/accept", post(friends::accept))
        // sync / backup
        .route("/sync/manifest", post(sync::manifest))
        .route("/sync/track/:sha", put(sync::upload).head(sync::have).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)))
        .route("/sync/status", get(sync::status))
        .route("/backup/snapshot", post(sync::snapshot))
        .route("/backup/snapshots", get(sync::snapshots))
        .route("/backup/blob/:name", put(sync::put_blob).get(sync::get_blob).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)))
        .route("/backup/blobs", get(sync::blobs))
        // library
        .route("/library", get(library::list))
        .route("/library/import", post(library::import))
        .route("/library/analyse/:sha", post(library::analyse))
        .route("/track/:sha", get(library::stream))
        .route("/track/:sha/meta", get(library::meta))
        .route("/track/:sha/cover", get(library::cover))
        .route("/track/:sha/lyrics", get(library::lyrics))
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
        .route("/player/karaoke", post(player::karaoke))
        // shared queue (friends on the LAN) and the listening ledger
        .route("/party", post(party::party))
        .route("/ledger", get(party::ledger).delete(party::clear))
        .route("/ledger/export", get(party::export))
        // self-update and patch notes
        .route("/update", get(update::get))
        .route("/update/check", post(update::check))
        .route("/update/stage", post(update::stage))
        .route("/update/install", post(update::install))
        .route("/update/discard", post(update::discard))
        .route("/update/seen", post(update::seen))
        .route("/update/notes", get(update::notes))
        // stations
        .route("/identity", get(stations::identity))
        .route("/stations/follows", get(stations::follows).post(stations::follow))
        .route("/stations/follows/:pubkey", delete(stations::unfollow))
        .route("/stations/feed", get(stations::feed))
        .route("/stations/host", get(stations::host_status).post(stations::host))
        .route("/stations/link", get(stations::my_link))
        .route("/stations/listen", get(stations::listen_status).post(stations::listen).delete(stations::stop_listening))
        .route("/relay/stats", get(stations::relay_stats))
        .route("/funnel", get(funnel));

    Router::new()
        .nest("/api", api)
        .route("/share/:sha/:cap", get(library::share_download))
        .route("/share/:sha/:cap/cover", get(library::share_cover))
        .route("/share/:sha/:cap/lyrics", get(library::share_lyrics))
        .route("/relay", get(crate::relay_ws::handler))
        .route("/ws/events", get(player::events_ws))
        // companion API at the root, exactly where the phone's Companion backend expects it
        .route("/health", get(companion::health))
        .route("/jobs", post(companion::create_job).get(companion::list_jobs))
        .route("/jobs/:id", get(companion::job).delete(companion::delete_job))
        .route("/files/:name", get(companion::file))
        .fallback(crate::ui::serve)
        .layer(middleware::from_fn_with_state(state.clone(), policy))
        .layer(Extension(origin))
        .with_state(state)
}

async fn status(_u: AnyUser, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
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
        "identity": { "npub": s.identity().npub(), "pubkey": s.identity().pubkey_hex() },
        "account": s.account.summary(),
        "relayEnabled": s.relay_enabled(),
        "shared": s.account.shared(),
        "listening": s.listen.status(),
        "companion": companion,
        "audio": s.player.describe(),
        "pendingPairs": s.pending_pair_count(),
        "update": { "applied": crate::update::read_applied(&s.data_dir).is_some(), "staged": crate::update::read_staged(&s.data_dir).map(|st| st.version), "notesSeen": s.settings().notes_seen_version == crate::update::CURRENT },
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
        cur.karaoke_strength = if cur.karaoke_strength.is_nan() { 1.0 } else { cur.karaoke_strength.clamp(0.0, 1.0) };
    })?;
    s.stations.poke();
    Ok(Json(updated))
}

async fn funnel(_u: AnyUser, State(s): State<App>) -> Json<serde_json::Value> {
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
        .header(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")))
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

    fn headers(pairs: &[(&str, &str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn admin_needs_a_loopback_host() {
        use super::host_is_loopback;
        assert!(host_is_loopback(&headers(&[("host", "localhost:8770")])));
        assert!(host_is_loopback(&headers(&[("host", "127.0.0.1:8770")])));
        assert!(host_is_loopback(&headers(&[("host", "[::1]:8770")])));
        assert!(host_is_loopback(&headers(&[("host", "LOCALHOST")])));
        // DNS rebinding: a name that happens to resolve to 127.0.0.1 is not the desktop UI.
        assert!(!host_is_loopback(&headers(&[("host", "evil.example:8770")])));
        assert!(!host_is_loopback(&headers(&[("host", "192.168.1.5:8770")])));
        assert!(!host_is_loopback(&headers(&[])));
    }

    #[test]
    fn cross_site_browser_writes_are_refused() {
        use super::same_site_or_native;
        // curl, the phone, the Python simulator: no browser headers at all.
        assert!(same_site_or_native(&headers(&[("host", "localhost:8770")])));
        // The UI's own fetch.
        assert!(same_site_or_native(&headers(&[("host", "localhost:8770"), ("origin", "http://localhost:8770"), ("sec-fetch-site", "same-origin")])));
        assert!(same_site_or_native(&headers(&[("host", "127.0.0.1:8770"), ("origin", "http://127.0.0.1:8770")])));
        // A page on another site poking at localhost.
        assert!(!same_site_or_native(&headers(&[("host", "localhost:8770"), ("origin", "https://evil.example"), ("sec-fetch-site", "cross-site")])));
        assert!(!same_site_or_native(&headers(&[("host", "localhost:8770"), ("origin", "https://evil.example")])));
        assert!(!same_site_or_native(&headers(&[("host", "localhost:8770"), ("origin", "null")])));
        assert!(!same_site_or_native(&headers(&[("host", "localhost:8770"), ("sec-fetch-site", "cross-site")])));
    }
}
