//! The companion download API at the root, proxied to the Python worker, so the
//! phone's Downloads → Companion (LAN) setting takes `http://<desktop>:8770`.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::{ApiError, ApiResult, App};

fn base(s: &App) -> ApiResult<String> {
    let c = s.companion.lock().unwrap_or_else(|p| p.into_inner()).clone();
    match c {
        Some(c) if c.running => Ok(format!("http://127.0.0.1:{}", c.port)),
        Some(c) => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, c.error.unwrap_or_else(|| "download worker is not running".into()))),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "download worker is starting".into())),
    }
}

async fn passthrough(s: &App, resp: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
    let len = resp.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).map(str::to_string);
    let _ = s;
    let mut b = Response::builder().status(status).header(header::CONTENT_TYPE, ct);
    if let Some(l) = len {
        b = b.header(header::CONTENT_LENGTH, l);
    }
    b.body(Body::from_stream(resp.bytes_stream())).unwrap()
}

pub async fn health(State(s): State<App>) -> Response {
    match base(&s) {
        Ok(b) => match s.http.get(format!("{b}/health")).send().await {
            Ok(r) => passthrough(&s, r).await,
            Err(e) => ApiError(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        },
        // The phone's "Test" button: say so plainly, but keep the daemon itself reachable.
        Err(e) => e.into_response(),
    }
}

pub async fn create_job(State(s): State<App>, body: String) -> ApiResult<Response> {
    let b = base(&s)?;
    let mut v: serde_json::Value = serde_json::from_str(&body).map_err(|_| ApiError::bad("bad json"))?;
    if v.get("format").and_then(|f| f.as_str()).map(|f| f.is_empty()).unwrap_or(true) {
        v["format"] = serde_json::json!(s.settings().companion_format);
    }
    let r = s.http.post(format!("{b}/jobs")).json(&v).send().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(passthrough(&s, r).await)
}

pub async fn list_jobs(State(s): State<App>) -> ApiResult<Response> {
    let b = base(&s)?;
    let r = s.http.get(format!("{b}/jobs")).send().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(passthrough(&s, r).await)
}

pub async fn job(State(s): State<App>, Path(id): Path<String>) -> ApiResult<Response> {
    let b = base(&s)?;
    let r = s.http.get(format!("{b}/jobs/{}", urlenc(&id))).send().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(passthrough(&s, r).await)
}

pub async fn delete_job(State(s): State<App>, Path(id): Path<String>) -> ApiResult<Response> {
    let b = base(&s)?;
    let r = s.http.delete(format!("{b}/jobs/{}", urlenc(&id))).send().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(passthrough(&s, r).await)
}

pub async fn file(State(s): State<App>, Path(name): Path<String>) -> ApiResult<Response> {
    let b = base(&s)?;
    let r = s.http.get(format!("{b}/files/{}", urlenc(&name))).send().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(passthrough(&s, r).await)
}

fn urlenc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[allow(dead_code)]
fn _json(v: serde_json::Value) -> Json<serde_json::Value> {
    Json(v)
}
