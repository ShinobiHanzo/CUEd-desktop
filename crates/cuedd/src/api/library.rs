//! The desktop library: listing, range streaming, share-to-phone QR,
//! pulling a share from a phone, importing local files.

use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use cued_proto::SharePayload;

use super::{Admin, AnyUser, ApiError, ApiResult, App};
use crate::db::Track;
use crate::state::UiEvent;

#[derive(serde::Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub q: String,
    pub device: Option<String>,
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}
fn default_limit() -> i64 {
    200
}

pub async fn list(_u: AnyUser, State(s): State<App>, Query(q): Query<ListQuery>) -> ApiResult<Json<serde_json::Value>> {
    let (tracks, total) = s.db.tracks(&q.q, q.device.as_deref().filter(|d| !d.trim().is_empty()), q.offset.max(0), q.limit.clamp(1, 1000))?;
    Ok(Json(serde_json::json!({ "tracks": tracks, "total": total })))
}

pub async fn meta(_u: AnyUser, State(s): State<App>, Path(sha): Path<String>) -> ApiResult<Json<Track>> {
    Ok(Json(s.db.track(&sha.to_ascii_lowercase())?.ok_or_else(|| ApiError::not_found("no such track"))?))
}

pub async fn stream(_u: AnyUser, State(s): State<App>, Path(sha): Path<String>, headers: HeaderMap) -> ApiResult<Response> {
    let t = s.db.track(&sha.to_ascii_lowercase())?.ok_or_else(|| ApiError::not_found("no such track"))?;
    super::range_file(std::path::Path::new(&t.path), &t.mime, headers.get(header::RANGE), None).await
}

#[derive(serde::Deserialize, Default)]
pub struct ShareBody {
    /// "Send a friend request with it": adds this account's key and name to the link.
    #[serde(default)]
    pub friend: bool,
}

/// `POST /api/share/{sha}`: a `cued://share` link the phone's Receive screen pulls from, no pairing needed.
pub async fn share(_a: Admin, State(s): State<App>, Path(sha): Path<String>, body: Option<Json<ShareBody>>) -> ApiResult<Json<serde_json::Value>> {
    let sha = sha.to_ascii_lowercase();
    let t = s.db.track(&sha)?.ok_or_else(|| ApiError::not_found("no such track"))?;
    let cap = s.new_share_cap(&sha);
    let base = s.lan_urls().into_iter().next().unwrap_or_else(|| format!("http://127.0.0.1:{}", s.port));
    let mut p = SharePayload::new(t.title.clone(), t.artist.clone());
    p.link = t.link.clone();
    p.file_url = Some(format!("{base}/share/{sha}/{cap}"));
    p.genres = t.genres.clone();
    p.bpm = t.bpm.map(|b| b as f32);
    if body.map(|b| b.friend).unwrap_or(false) {
        p.friend_pubkey = Some(s.identity().pubkey_hex());
        p.friend_name = Some(s.account.chain().name());
    }
    let link = p.encode();
    Ok(Json(serde_json::json!({ "link": link, "web": p.encode_web(), "svg": super::pair::qr_svg(&link)?, "expiresInSec": 1800 })))
}

pub async fn share_download(State(s): State<App>, Path((sha, cap)): Path<(String, String)>, headers: HeaderMap) -> ApiResult<Response> {
    let sha = sha.to_ascii_lowercase();
    if !s.check_share_cap(&sha, &cap) {
        return Err(ApiError::forbidden("share link expired; make a new one on the desktop"));
    }
    let t = s.db.track(&sha)?.ok_or_else(|| ApiError::not_found("no such track"))?;
    let ext = crate::store::ext_for_mime(&t.mime, "m4a");
    let name = format!("{} - {}.{}", crate::store::safe_segment(&t.artist), crate::store::safe_segment(&t.title), ext);
    super::range_file(std::path::Path::new(&t.path), &t.mime, headers.get(header::RANGE), Some(&name)).await
}

pub async fn share_cover(State(s): State<App>, Path((sha, cap)): Path<(String, String)>) -> ApiResult<Response> {
    let sha = sha.to_ascii_lowercase();
    if !s.check_share_cap(&sha, &cap) {
        return Err(ApiError::forbidden("share link expired"));
    }
    cover_response(&s, &sha).await
}

pub async fn share_lyrics(State(s): State<App>, Path((sha, cap)): Path<(String, String)>) -> ApiResult<Json<serde_json::Value>> {
    let sha = sha.to_ascii_lowercase();
    if !s.check_share_cap(&sha, &cap) {
        return Err(ApiError::forbidden("share link expired"));
    }
    lyrics_json(&s, &sha).await
}

/// `GET /api/track/{sha}/cover`: the embedded picture, extracted once with ffmpeg and cached.
pub async fn cover(_u: AnyUser, State(s): State<App>, Path(sha): Path<String>) -> ApiResult<Response> {
    cover_response(&s, &sha.to_ascii_lowercase()).await
}

async fn cover_response(s: &App, sha: &str) -> ApiResult<Response> {
    let t = s.db.track(sha)?.ok_or_else(|| ApiError::not_found("no such track"))?;
    let path = crate::probe::cover_file(&s.data_dir, sha, std::path::Path::new(&t.path)).await.ok_or_else(|| ApiError::not_found("no cover in this file"))?;
    let mut r = super::range_file(&path, "image/jpeg", None, None).await?;
    r.headers_mut().insert(header::CACHE_CONTROL, "max-age=86400".parse().unwrap());
    Ok(r)
}

/// `GET /api/track/{sha}/lyrics`: `.lrc` sidecar, else the embedded lyrics tag.
pub async fn lyrics(_u: AnyUser, State(s): State<App>, Path(sha): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    lyrics_json(&s, &sha.to_ascii_lowercase()).await
}

async fn lyrics_json(s: &App, sha: &str) -> ApiResult<Json<serde_json::Value>> {
    let t = s.db.track(sha)?.ok_or_else(|| ApiError::not_found("no such track"))?;
    let (text, synced) = crate::probe::lyrics(std::path::Path::new(&t.path)).await.ok_or_else(|| ApiError::not_found("no lyrics for this track"))?;
    Ok(Json(serde_json::json!({ "text": text, "synced": synced })))
}

#[derive(serde::Deserialize)]
pub struct ReceiveBody {
    pub text: String,
}

/// `POST /api/receive {"text": "cued://share?…"}`: pull a track a phone is sharing right now.
pub async fn receive(_a: Admin, State(s): State<App>, Json(b): Json<ReceiveBody>) -> ApiResult<Json<serde_json::Value>> {
    let payload = SharePayload::decode(&b.text).map_err(|e| ApiError::bad(format!("not a share link: {e}")))?;
    // "Send a friend request with it": the sender's key rides in the link.
    let friend = match (&payload.friend_pubkey, &payload.friend_name) {
        (Some(pk), name) if pk != &s.identity().pubkey_hex() => crate::friends::request(&s, pk, name.as_deref().unwrap_or(""), &[]).ok(),
        _ => None,
    };
    let Some(file_url) = payload.file_url.clone() else {
        return Ok(Json(serde_json::json!({ "status": "no file in this share", "payload": payload, "link": payload.link })));
    };
    let resp = s.http.get(&file_url).send().await.map_err(|e| ApiError::bad(format!("could not reach the phone: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::bad(format!("phone answered {}", resp.status())));
    }
    let mime = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("audio/mpeg").to_string();
    let ext = crate::store::ext_for_mime(&mime, "mp3");
    let tmp = s.store.downloads_dir().join(format!("receive-{}.{ext}", hex::encode(rand::random::<[u8; 6]>())));
    let bytes = resp.bytes().await.map_err(|e| ApiError::bad(format!("transfer failed: {e}")))?;
    tokio::fs::write(&tmp, &bytes).await?;
    // A CUEd share server also serves /meta/<id> next to /track/<id>.
    let mut genres = payload.genres.clone();
    let mut album = String::new();
    let mut link = payload.link.clone();
    if let Some(meta_url) = file_url.rsplit_once("/track/").map(|(base, id)| format!("{base}/meta/{id}")) {
        if let Ok(r) = s.http.get(&meta_url).send().await {
            if let Ok(m) = r.json::<serde_json::Value>().await {
                if let Some(a) = m["album"].as_str() {
                    album = a.to_string();
                }
                if let Some(g) = m["genres"].as_array() {
                    genres = g.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                }
                if link.is_none() {
                    link = m["link"].as_str().map(str::to_string);
                }
            }
        }
    }
    let track = import_file(&s, tmp, Some((payload.title.clone(), payload.artist.clone(), album)), genres, payload.bpm.map(|b| b as f64), link, "receive").await?;
    Ok(Json(serde_json::json!({ "status": "added", "track": track, "friend": friend })))
}

#[derive(serde::Deserialize)]
pub struct ImportBody {
    pub path: String,
}

/// UI: import a local file or folder into the library (moves nothing: copies).
pub async fn import(_a: Admin, State(s): State<App>, Json(b): Json<ImportBody>) -> ApiResult<Json<serde_json::Value>> {
    let root = PathBuf::from(b.path.trim());
    let mut files = Vec::new();
    collect_audio(&root, &mut files, 0);
    if files.is_empty() {
        return Err(ApiError::bad("no audio files there"));
    }
    let mut added = 0;
    let mut skipped = 0;
    for f in files {
        let tmp = s.store.downloads_dir().join(format!("import-{}-{}", hex::encode(rand::random::<[u8; 4]>()), f.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
        if tokio::fs::copy(&f, &tmp).await.is_err() {
            skipped += 1;
            continue;
        }
        match import_file(&s, tmp, None, vec![], None, None, "import").await {
            Ok(t) => {
                added += 1;
                // A `.lrc` sidecar travels with its track.
                let lrc = f.with_extension("lrc");
                if lrc.is_file() {
                    let _ = tokio::fs::copy(&lrc, std::path::Path::new(&t.path).with_extension("lrc")).await;
                }
            }
            Err(_) => skipped += 1,
        }
    }
    Ok(Json(serde_json::json!({ "added": added, "skipped": skipped })))
}

fn collect_audio(p: &std::path::Path, out: &mut Vec<PathBuf>, depth: usize) {
    const EXT: [&str; 7] = ["mp3", "m4a", "flac", "ogg", "opus", "wav", "aac"];
    if p.is_file() {
        if p.extension().and_then(|e| e.to_str()).map(|e| EXT.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false) {
            out.push(p.to_path_buf());
        }
    } else if p.is_dir() && depth < 6 {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                collect_audio(&e.path(), out, depth + 1);
            }
        }
    }
}

/// Hashes `file`, moves it into the library and indexes it. `known` is (title, artist, album) when the caller has it.
pub async fn import_file(s: &App, file: PathBuf, known: Option<(String, String, String)>, genres: Vec<String>, bpm: Option<f64>, link: Option<String>, source: &str) -> ApiResult<Track> {
    let probed = crate::probe::probe(&file).await;
    let (title, artist, album) = match known {
        Some((t, a, al)) => (if t.is_empty() { probed.title.clone() } else { t }, if a.is_empty() { probed.artist.clone() } else { a }, if al.is_empty() { probed.album.clone() } else { al }),
        None => (probed.title.clone(), probed.artist.clone(), probed.album.clone()),
    };
    let (sha, dest, size) = s.store.adopt(&file, &artist, &album, &title).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(existing) = s.db.track(&sha)? {
        // Same bytes already in the library: keep the first copy, drop the duplicate file.
        if existing.path != dest.to_string_lossy() {
            let _ = tokio::fs::remove_file(&dest).await;
        }
        return Ok(existing);
    }
    let track = Track {
        sha256: sha.clone(),
        title,
        artist,
        album,
        duration_ms: probed.duration_ms,
        size: size as i64,
        mime: crate::store::mime_for_path(&dest),
        genres: if genres.is_empty() { probed.genres } else { genres },
        bpm,
        link: link.or(probed.link),
        path: dest.to_string_lossy().into_owned(),
        added_at: crate::db::now_ms(),
        source: source.into(),
        devices: vec![],
    };
    s.db.add_track(&track)?;
    s.emit(UiEvent::TrackAdded { sha256: sha.clone(), title: track.title.clone(), artist: track.artist.clone(), source: source.into() });
    if track.bpm.is_none() {
        crate::player::analyse_later(s.clone(), sha, dest);
    }
    Ok(s.db.track(&track.sha256)?.unwrap_or(track))
}

pub async fn analyse(_a: Admin, State(s): State<App>, Path(sha): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let t = s.db.track(&sha.to_ascii_lowercase())?.ok_or_else(|| ApiError::not_found("no such track"))?;
    let a = crate::player::analyse_now(&t.path).await.map_err(|e| ApiError::bad(format!("analysis failed: {e}")))?;
    s.db.set_bpm(&t.sha256, a["bpm"].as_f64().unwrap_or(0.0))?;
    Ok(Json(a))
}

#[derive(serde::Deserialize)]
pub struct QrQuery {
    pub text: String,
}

/// Any text as a QR SVG (the UI uses it for station follow links).
pub async fn qr(_a: Admin, Query(q): Query<QrQuery>) -> ApiResult<Response> {
    let svg = super::pair::qr_svg(&q.text)?;
    Ok(Response::builder().header(header::CONTENT_TYPE, "image/svg+xml").body(svg.into()).unwrap())
}
