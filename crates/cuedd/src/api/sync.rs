//! Manifest, resumable upload, backup snapshots and blobs. `docs/protocol.md` §3.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;

use super::{ApiError, ApiResult, App, Phone};
use crate::db::{ManifestEntry, Track};
use crate::state::UiEvent;
use crate::store::{ext_for_mime, StoreError};

#[derive(serde::Deserialize)]
pub struct ManifestBody {
    pub tracks: Vec<ManifestEntry>,
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub async fn manifest(Phone(dev): Phone, State(s): State<App>, Json(body): Json<ManifestBody>) -> ApiResult<Json<serde_json::Value>> {
    let mut entries = body.tracks;
    for e in &mut entries {
        e.sha256 = e.sha256.to_ascii_lowercase();
        if !valid_sha(&e.sha256) {
            return Err(ApiError::bad(format!("bad sha256 for {:?}", e.title)));
        }
    }
    let missing = s.db.set_manifest(&dev.id, &entries)?;
    s.db.touch_device(&dev.id, true, false)?;
    tracing::info!("{}: manifest with {} tracks, {} missing here", dev.name, entries.len(), missing.len());
    Ok(Json(serde_json::json!({ "missing": missing, "known": entries.len() as i64 - missing.len() as i64 })))
}

pub async fn have(_p: Phone, State(s): State<App>, Path(sha): Path<String>) -> ApiResult<Response> {
    let sha = sha.to_ascii_lowercase();
    if !valid_sha(&sha) {
        return Err(ApiError::bad("bad sha256"));
    }
    if s.db.has_track(&sha)? {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let have = s.store.have(&sha).await;
    Ok((StatusCode::OK, [("X-Cued-Have", have.to_string())]).into_response())
}

/// `Content-Range: bytes <start>-<end>/<total>` → (start, total).
fn content_range(h: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let v = h.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let start: u64 = range.split('-').next()?.parse().ok()?;
    let total = total.trim().parse::<u64>().ok();
    Some((start, total))
}

pub async fn upload(Phone(dev): Phone, State(s): State<App>, Path(sha): Path<String>, headers: HeaderMap, body: Body) -> ApiResult<Response> {
    let sha = sha.to_ascii_lowercase();
    if !valid_sha(&sha) {
        return Err(ApiError::bad("bad sha256"));
    }
    if s.db.has_track(&sha)? {
        s.db.link_device_track(&dev.id, &meta_for(&s, &dev.id, &sha, &headers)?)?;
        return Ok((StatusCode::CREATED, Json(serde_json::json!({ "status": "already here" }))).into_response());
    }
    let (start, total) = content_range(&headers).unwrap_or((0, None));
    let total = total.or_else(|| headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).map(|len: u64| start + len));
    let mut file = match s.store.begin(&sha, start).await {
        Ok(f) => f,
        Err(StoreError::BadOffset { have, .. }) => {
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, [("X-Cued-Have", have.to_string())], Json(serde_json::json!({ "error": "resume from X-Cued-Have" }))).into_response());
        }
        Err(e) => return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let mut written = start;
    let mut stream = body.into_data_stream();
    let mut last_emit = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::bad(format!("body: {e}")))?;
        crate::store::write_all(&mut file, &chunk).await?;
        written += chunk.len() as u64;
        if last_emit.elapsed().as_millis() > 300 {
            s.emit(UiEvent::Upload { sha256: sha.clone(), have: written, total: total.unwrap_or(0) });
            last_emit = std::time::Instant::now();
        }
    }
    drop(file);

    let complete = match total {
        Some(t) => written >= t,
        None => true,
    };
    if !complete {
        return Ok((StatusCode::ACCEPTED, [("X-Cued-Have", written.to_string())], Json(serde_json::json!({ "have": written, "total": total }))).into_response());
    }

    let meta = meta_for(&s, &dev.id, &sha, &headers)?;
    let ext = ext_for_mime(&meta.mime, "m4a");
    let dest = match s.store.finish(&sha, &meta.artist, &meta.album, &meta.title, ext).await {
        Ok(p) => p,
        Err(StoreError::HashMismatch { expected, got }) => {
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": "hash mismatch", "expected": expected, "got": got }))).into_response());
        }
        Err(e) => return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let size = tokio::fs::metadata(&dest).await?.len() as i64;
    let track = Track {
        sha256: sha.clone(),
        title: meta.title.clone(),
        artist: meta.artist.clone(),
        album: meta.album.clone(),
        duration_ms: meta.duration_ms,
        size,
        mime: if meta.mime.is_empty() { crate::store::mime_for_path(&dest) } else { meta.mime.clone() },
        genres: meta.genres.clone(),
        bpm: meta.bpm,
        link: meta.link.clone(),
        path: dest.to_string_lossy().into_owned(),
        added_at: crate::db::now_ms(),
        source: format!("phone:{}", dev.id),
        devices: vec![],
    };
    s.db.add_track(&track)?;
    s.db.link_device_track(&dev.id, &meta)?;
    s.db.touch_device(&dev.id, false, true)?;
    s.emit(UiEvent::TrackAdded { sha256: sha.clone(), title: track.title.clone(), artist: track.artist.clone(), source: track.source.clone() });
    s.emit(UiEvent::Upload { sha256: sha.clone(), have: size as u64, total: size as u64 });
    tracing::info!("{}: backed up {} - {} ({} bytes)", dev.name, track.artist, track.title, size);
    if track.bpm.is_none() {
        crate::player::analyse_later(s.clone(), sha.clone(), dest);
    }
    Ok((StatusCode::CREATED, Json(s.db.track(&sha)?)).into_response())
}

/// Metadata for an upload: the `X-Cued-Meta` header, else the device's manifest entry, else a stub.
fn meta_for(s: &App, device: &str, sha: &str, headers: &HeaderMap) -> ApiResult<ManifestEntry> {
    if let Some(v) = headers.get("x-cued-meta").and_then(|v| v.to_str().ok()) {
        let mut m: ManifestEntry = serde_json::from_str(v).map_err(|e| ApiError::bad(format!("X-Cued-Meta: {e}")))?;
        m.sha256 = sha.to_string();
        return Ok(m);
    }
    if let Some(m) = s.db.manifest_entry(device, sha)? {
        return Ok(m);
    }
    Ok(ManifestEntry { sha256: sha.into(), title: sha[..12].into(), mime: "audio/mp4".into(), ..Default::default() })
}

pub async fn status(Phone(dev): Phone, State(s): State<App>) -> ApiResult<Json<crate::db::DeviceStatus>> {
    Ok(Json(s.db.device_status(&dev)?))
}

pub async fn snapshot(Phone(dev): Phone, State(s): State<App>) -> ApiResult<Json<serde_json::Value>> {
    let entries = s.db.manifest(&dev.id)?;
    let dir = s.store.backup_dir(&dev.id);
    tokio::fs::create_dir_all(&dir).await?;
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let path = dir.join(format!("{stamp}.json"));
    let body = serde_json::json!({ "device": dev, "takenAt": crate::db::now_ms(), "tracks": entries });
    tokio::fs::write(&path, serde_json::to_vec_pretty(&body)?).await?;
    Ok(Json(serde_json::json!({ "file": path.file_name().and_then(|f| f.to_str()), "tracks": entries.len() })))
}

pub async fn snapshots(Phone(dev): Phone, State(s): State<App>) -> ApiResult<Json<Vec<serde_json::Value>>> {
    Ok(Json(list_snapshots(&s, &dev.id).await?))
}

pub async fn list_snapshots(s: &App, device: &str) -> ApiResult<Vec<serde_json::Value>> {
    let dir = s.store.backup_dir(device);
    let mut out = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                let size = e.metadata().await.map(|m| m.len()).unwrap_or(0);
                out.push(serde_json::json!({ "file": name, "size": size }));
            }
        }
    }
    out.sort_by(|a, b| b["file"].as_str().cmp(&a["file"].as_str()));
    Ok(out)
}

fn valid_blob_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 80 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-') && !n.starts_with('.')
}

pub async fn put_blob(Phone(dev): Phone, State(s): State<App>, Path(name): Path<String>, body: Body) -> ApiResult<StatusCode> {
    if !valid_blob_name(&name) {
        return Err(ApiError::bad("blob name must be [A-Za-z0-9._-]{1,80}"));
    }
    let path = s.store.blob_path(&dev.id, &name);
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    let tmp = path.with_extension("tmp");
    let mut f = tokio::fs::File::create(&tmp).await?;
    let mut stream = body.into_data_stream();
    let mut size = 0i64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::bad(format!("body: {e}")))?;
        if size + chunk.len() as i64 > 256 << 20 {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "blobs are capped at 256 MB".into()));
        }
        crate::store::write_all(&mut f, &chunk).await?;
        size += chunk.len() as i64;
    }
    drop(f);
    tokio::fs::rename(&tmp, &path).await?;
    s.db.put_blob(&dev.id, &name, size)?;
    Ok(StatusCode::CREATED)
}

pub async fn get_blob(Phone(dev): Phone, State(s): State<App>, Path(name): Path<String>) -> ApiResult<Response> {
    if !valid_blob_name(&name) {
        return Err(ApiError::bad("bad blob name"));
    }
    let path = s.store.blob_path(&dev.id, &name);
    super::range_file(&path, "application/octet-stream", None, Some(&name)).await
}

pub async fn blobs(Phone(dev): Phone, State(s): State<App>) -> ApiResult<Json<Vec<serde_json::Value>>> {
    Ok(Json(s.db.blobs(&dev.id)?))
}
