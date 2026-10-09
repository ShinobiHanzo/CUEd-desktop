//! The web UI, embedded at build time so the binary is self-contained.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

const INDEX: &str = include_str!("../../../ui/index.html");
const APP_JS: &str = include_str!("../../../ui/app.js");
const STYLE: &str = include_str!("../../../ui/style.css");
const ICON: &str = include_str!("../../../ui/icon.svg");

pub async fn serve(uri: Uri) -> Response {
    let (body, mime): (&str, &str) = match uri.path() {
        "/" | "/index.html" => (INDEX, "text/html; charset=utf-8"),
        "/app.js" => (APP_JS, "application/javascript; charset=utf-8"),
        "/style.css" => (STYLE, "text/css; charset=utf-8"),
        "/icon.svg" => (ICON, "image/svg+xml"),
        p if p.starts_with("/api/") => return (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
        _ => (INDEX, "text/html; charset=utf-8"),
    };
    ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}
