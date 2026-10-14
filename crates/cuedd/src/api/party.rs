//! `POST /api/party` (friends' phones on the LAN, signed envelopes) and the
//! listening ledger (`GET /api/ledger`, `GET /api/ledger/export`).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;

use super::{AnyUser, ApiError, ApiResult, App};
use crate::party::{self, Envelope};

/// The party endpoint. Anyone on the LAN may post; the envelope's signature decides.
pub async fn party(State(s): State<App>, body: String) -> ApiResult<Json<Envelope>> {
    if body.len() > 8 * 1024 {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "party messages are small".into()));
    }
    let e: Envelope = serde_json::from_str(&body).map_err(|_| ApiError::bad("not a party envelope"))?;
    Ok(Json(party::handle(&s, &e)))
}

#[derive(serde::Deserialize)]
pub struct LedgerQuery {
    /// `month` (default), `last-month`, `year`, `7d`, `30d`, `all`, or explicit `from`/`to` millis.
    pub period: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub top: Option<usize>,
}

fn range(q: &LedgerQuery) -> (i64, i64) {
    match (q.from, q.to) {
        (Some(a), Some(b)) if a < b => (a, b),
        _ => crate::ledger::period(q.period.as_deref().unwrap_or("month"), crate::db::now_ms()),
    }
}

pub async fn ledger(_u: AnyUser, State(s): State<App>, Query(q): Query<LedgerQuery>) -> ApiResult<Json<serde_json::Value>> {
    let (from, to) = range(&q);
    let rows = s.db.plays_all()?;
    let summary = crate::ledger::summarise(&rows, from, to, crate::db::now_ms(), q.top.unwrap_or(10).clamp(1, 50));
    Ok(Json(serde_json::json!({ "period": q.period.unwrap_or_else(|| "month".into()), "summary": summary, "enabled": s.settings().ledger })))
}

/// The whole log as a file you own: `cued-ledger/1`, the same shape the phone exports.
pub async fn export(_u: AnyUser, State(s): State<App>, Query(q): Query<LedgerQuery>) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let (from, to) = range(&q);
    let rows = s.db.plays_all()?;
    let summary = crate::ledger::summarise(&rows, from, to, crate::db::now_ms(), 50);
    let plays: Vec<serde_json::Value> = rows
        .iter()
        .filter(|p| p.started_at >= from && p.started_at < to)
        .map(|p| serde_json::json!({ "trackKey": p.sha256, "at": p.started_at, "skipped": if p.duration_ms > 0 { p.listened_ms * 2 < p.duration_ms } else { !p.completed }, "completed": p.completed, "title": p.title, "artist": p.artist, "album": p.album, "durationMs": p.duration_ms, "genres": p.genres, "listenedMs": p.listened_ms }))
        .collect();
    let body = serde_json::to_vec_pretty(&serde_json::json!({ "format": "cued-ledger/1", "summary": summary, "plays": plays }))?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json"), (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"cued-ledger.json\"")],
        body,
    )
        .into_response())
}

#[derive(serde::Deserialize)]
pub struct ClearBody {
    pub before: Option<i64>,
}

/// Forget plays (all, or those before a time). Yours to erase, so it is one call.
pub async fn clear(_a: super::Admin, State(s): State<App>, Json(b): Json<ClearBody>) -> ApiResult<Json<serde_json::Value>> {
    let n = s.db.plays_clear(b.before)?;
    Ok(Json(serde_json::json!({ "removed": n })))
}
