//! Desktop playback controls and the live event socket the UI listens on.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::Json;

use super::{Admin, ApiError, ApiResult, App};

pub async fn status(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    Json(s.player.status())
}

#[derive(serde::Deserialize)]
pub struct ShaBody {
    pub sha256: String,
}

pub async fn load(_a: Admin, State(s): State<App>, Json(b): Json<ShaBody>) -> ApiResult<Json<serde_json::Value>> {
    let t = s.db.track(&b.sha256.to_ascii_lowercase())?.ok_or_else(|| ApiError::not_found("no such track"))?;
    if !s.player.enabled() {
        return Err(ApiError::bad("audio engine is off (--no-audio or no device)"));
    }
    s.player.load(&t).map_err(|e| ApiError::bad(format!("could not load: {e}")))?;
    Ok(Json(s.player.status()))
}

pub async fn play(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    s.player.play();
    Json(s.player.status())
}
pub async fn pause(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    s.player.pause();
    Json(s.player.status())
}
pub async fn stop(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    s.player.stop();
    Json(s.player.status())
}

#[derive(serde::Deserialize)]
pub struct SeekBody {
    #[serde(rename = "positionSec")]
    pub position_sec: f64,
}
pub async fn seek(_a: Admin, State(s): State<App>, Json(b): Json<SeekBody>) -> Json<serde_json::Value> {
    s.player.seek(b.position_sec);
    Json(s.player.status())
}

#[derive(serde::Deserialize)]
pub struct VolumeBody {
    pub volume: f32,
}
pub async fn volume(_a: Admin, State(s): State<App>, Json(b): Json<VolumeBody>) -> Json<serde_json::Value> {
    s.player.set_volume(b.volume);
    Json(s.player.status())
}

#[derive(serde::Deserialize)]
pub struct KaraokeBody {
    pub on: bool,
    pub strength: Option<f32>,
}
/// Karaoke switch; also persisted in settings so it survives a restart.
pub async fn karaoke(_a: Admin, State(s): State<App>, Json(b): Json<KaraokeBody>) -> ApiResult<Json<serde_json::Value>> {
    let strength = b.strength.unwrap_or_else(|| s.player.karaoke().1);
    s.update_settings(|cur| {
        cur.karaoke = b.on;
        cur.karaoke_strength = strength.clamp(0.0, 1.0);
    })?;
    Ok(Json(s.player.status()))
}

pub async fn queue(_a: Admin, State(s): State<App>) -> Json<Vec<crate::player::QueueItem>> {
    Json(s.player.queue())
}
pub async fn enqueue(_a: Admin, State(s): State<App>, Json(b): Json<ShaBody>) -> ApiResult<Json<Vec<crate::player::QueueItem>>> {
    let t = s.db.track(&b.sha256.to_ascii_lowercase())?.ok_or_else(|| ApiError::not_found("no such track"))?;
    s.player.enqueue(&t);
    Ok(Json(s.player.queue()))
}
pub async fn clear_queue(_a: Admin, State(s): State<App>) -> Json<Vec<crate::player::QueueItem>> {
    s.player.clear_queue();
    Json(s.player.queue())
}
pub async fn next(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    let advanced = s.player.next();
    let mut st = s.player.status();
    st["advanced"] = serde_json::json!(advanced);
    Json(st)
}

pub async fn spectrum(_a: Admin, State(s): State<App>) -> Json<Vec<f32>> {
    Json(s.player.spectrum())
}

/// `GET /ws/events`: player status + spectrum at ~15 Hz and every `UiEvent`, as JSON text frames.
pub async fn events_ws(_a: Admin, State(s): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| events_loop(socket, s))
}

async fn events_loop(mut socket: WebSocket, s: App) {
    let mut events = s.events.subscribe();
    let mut iv = tokio::time::interval(std::time::Duration::from_millis(66));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let frame = serde_json::json!({ "type": "player", "status": s.player.status(), "spectrum": s.player.spectrum() });
                if socket.send(Message::Text(frame.to_string())).await.is_err() { break; }
            }
            ev = events.recv() => {
                match ev {
                    Ok(e) => { if socket.send(Message::Text(serde_json::to_string(&e).unwrap_or_default())).await.is_err() { break; } }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg { Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break, _ => {} }
            }
        }
    }
}

#[allow(dead_code)]
pub async fn _unused(_: Path<String>) {}
