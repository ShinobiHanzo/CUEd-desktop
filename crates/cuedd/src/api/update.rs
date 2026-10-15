//! `/api/update`: what the updater knows, check now, install now, and the patch notes.

use axum::extract::{Query, State};
use axum::Json;

use super::{Admin, ApiError, ApiResult, App};
use crate::update;

fn snapshot(s: &App) -> serde_json::Value {
    let info = s.update.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let settings = s.settings();
    let applied = update::read_applied(&s.data_dir);
    let notes = applied.as_ref().map(|a| update::notes_between(update::CHANGELOG, Some(&a.from), &a.to));
    serde_json::json!({
        "current": update::CURRENT,
        "target": update::TARGET,
        "autoUpdate": settings.auto_update,
        "canInstall": update::can_install(),
        "info": info,
        "staged": update::read_staged(&s.data_dir),
        "applied": applied,
        "notes": notes,
        "notesSeen": applied.as_ref().map(|a| settings.notes_seen_version == a.to).unwrap_or(true),
        "applyError": update::read_apply_error(&s.data_dir),
    })
}

pub async fn get(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    Json(snapshot(&s))
}

/// Checks GitHub now and, when auto-update is on, stages what it finds.
pub async fn check(_a: Admin, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let stage = s.settings().auto_update;
    if let Err(e) = update::check(&s, stage).await {
        return Err(ApiError::bad(format!("update check failed: {e}")));
    }
    Ok(Json(snapshot(&s)))
}

/// Downloads and stages the latest even when auto-update is off (a deliberate click).
pub async fn stage(_a: Admin, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    if let Err(e) = update::check(&s, true).await {
        return Err(ApiError::bad(format!("update check failed: {e}")));
    }
    Ok(Json(snapshot(&s)))
}

/// Installs the staged version and restarts the daemon. The response may never arrive: the process re-executes.
pub async fn install(_a: Admin, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    if update::read_staged(&s.data_dir).is_none() {
        return Err(ApiError::bad("nothing is staged yet"));
    }
    if !update::can_install() {
        return Err(ApiError::bad("the folder holding cuedd is not writable; install the archive by hand"));
    }
    let data_dir = s.data_dir.clone();
    // Give the HTTP answer a moment to leave, then swap and re-exec from a plain thread.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Err(e) = update::apply_staged(&data_dir) {
            tracing::warn!("update: install now failed: {e}");
            let _ = std::fs::write(data_dir.join("updates").join("apply-error.txt"), e.to_string());
        }
    });
    Ok(Json(serde_json::json!({ "restarting": true })))
}

pub async fn discard(_a: Admin, State(s): State<App>) -> Json<serde_json::Value> {
    update::discard_staged(&s.data_dir);
    s.update.lock().unwrap_or_else(|p| p.into_inner()).staged = None;
    Json(snapshot(&s))
}

/// The patch notes have been read: stop showing them.
pub async fn seen(_a: Admin, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    s.update_settings(|cur| cur.notes_seen_version = update::CURRENT.to_string())?;
    update::forget_applied(&s.data_dir);
    Ok(Json(snapshot(&s)))
}

#[derive(serde::Deserialize)]
pub struct NotesQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Changelog sections between two versions (defaults: everything up to the running version).
pub async fn notes(_a: Admin, Query(q): Query<NotesQuery>) -> Json<serde_json::Value> {
    let to = q.to.unwrap_or_else(|| update::CURRENT.to_string());
    Json(serde_json::json!({ "to": to, "from": q.from, "markdown": update::notes_between(update::CHANGELOG, q.from.as_deref(), &to) }))
}
