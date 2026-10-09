//! Listening in on a friend's station (`docs/protocol.md` §10): resolve each
//! entry (library, host stream, downloader), play it at the host's position,
//! keep temporary audio and metadata for 48 hours.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cued_proto::{station, StationTrack};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::player::QueueItem;
use crate::state::{AppState, UiEvent};

pub const KEEP: Duration = Duration::from_secs(48 * 3600);
const DRIFT_MS: i64 = 3000;

#[derive(Default, Clone)]
pub struct ListenState {
    pub listening: Option<String>,
    pub name: String,
    pub status: String,
    pub message: String,
    pub now: Option<StationTrack>,
    pub drift_ms: i64,
    current_key: String,
}

#[derive(Default)]
pub struct ListenShared {
    inner: Mutex<ListenState>,
    /// Companion job ids that belong to listening: the importer must not move them into the library.
    pub temp_jobs: Mutex<HashSet<String>>,
}

impl ListenShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, ListenState> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn status(&self) -> Value {
        let s = self.lock();
        json!({
            "listening": s.listening,
            "name": s.name,
            "status": if s.listening.is_some() { s.status.clone() } else { "idle".into() },
            "message": s.message,
            "now": s.now,
            "driftMs": s.drift_ms,
        })
    }
    pub fn start(&self, pubkey: &str, name: &str) {
        let mut s = self.lock();
        *s = ListenState { listening: Some(pubkey.into()), name: name.into(), status: "resolving".into(), ..Default::default() };
    }
    pub fn stop(&self) {
        *self.lock() = ListenState::default();
    }
    pub fn listening(&self) -> Option<String> {
        self.lock().listening.clone()
    }
    fn set(&self, status: &str, message: impl Into<String>) {
        let mut s = self.lock();
        s.status = status.into();
        s.message = message.into();
    }
    pub fn is_temp_job(&self, id: &str) -> bool {
        self.temp_jobs.lock().unwrap_or_else(|p| p.into_inner()).contains(id)
    }
}

fn cache_dir(state: &AppState) -> PathBuf {
    state.data_dir.join("cache").join("stations")
}

fn ext_of(url: &str, fallback: &str) -> String {
    let path = url.split('?').next().unwrap_or("");
    path.rsplit('.').next().filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric())).map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| fallback.to_string())
}

/// Finds a file for `t`: library match, host stream, downloader. Returns the path and where it came from.
async fn resolve(state: &Arc<AppState>, t: &StationTrack) -> anyhow::Result<(PathBuf, &'static str)> {
    // 1. Something we already have.
    if let Ok((tracks, _)) = state.db.tracks(&t.title, None, 0, 20) {
        if let Some(hit) = tracks.into_iter().find(|x| x.title.eq_ignore_ascii_case(&t.title) && x.artist.eq_ignore_ascii_case(&t.artist)) {
            return Ok((PathBuf::from(hit.path), "library"));
        }
    }
    let dir = cache_dir(state);
    tokio::fs::create_dir_all(&dir).await?;
    let key = hex::encode(&sha2::Sha256::digest(format!("{}|{}|{}", t.artist, t.title, t.key).as_bytes())[..12]);
    // 2. The host's stream.
    if let Some(url) = &t.stream {
        let dest = dir.join(format!("{key}.{}", ext_of(url, "m4a")));
        if tokio::fs::metadata(&dest).await.map(|m| m.len() > 0).unwrap_or(false) {
            return Ok((dest, "cache"));
        }
        state.listen.set("resolving", format!("fetching from the host: {} - {}", t.artist, t.title));
        let resp = state.http.get(url).send().await?;
        let status = resp.status();
        if status.is_success() {
            let ext = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).map(|m| crate::store::ext_for_mime(m, "m4a")).unwrap_or("m4a");
            let bytes = resp.bytes().await?;
            let dest = dir.join(format!("{key}.{ext}"));
            tokio::fs::write(&dest, &bytes).await?;
            return Ok((dest, "stream"));
        }
        warn!("host stream answered {status}");
    }
    // 3. The source link through the downloader, as a temporary file.
    if let Some(link) = t.link.as_ref().filter(|l| l.starts_with("http")) {
        let base = {
            let c = state.companion.lock().unwrap_or_else(|p| p.into_inner()).clone();
            c.filter(|c| c.running).map(|c| format!("http://127.0.0.1:{}", c.port))
        };
        let Some(base) = base else { anyhow::bail!("no copy here, no host stream, and the download worker is off") };
        state.listen.set("resolving", format!("downloading {} - {}", t.artist, t.title));
        let created: Value = state.http.post(format!("{base}/jobs")).json(&json!({"url": link, "format": state.settings().companion_format})).send().await?.json().await?;
        let id = created["id"].as_str().unwrap_or("").to_string();
        if id.is_empty() {
            anyhow::bail!("downloader refused the link");
        }
        state.listen.temp_jobs.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone());
        for _ in 0..150 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let st: Value = state.http.get(format!("{base}/jobs/{id}")).send().await?.json().await?;
            match st["status"].as_str() {
                Some("done") => {
                    let file = st["files"].as_array().and_then(|a| a.iter().filter_map(|f| f.as_str()).find(|f| !f.ends_with(".lrc")));
                    let Some(name) = file else { anyhow::bail!("download produced no audio") };
                    let src = state.store.downloads_dir().join(crate::store::safe_segment(name));
                    let dest = dir.join(format!("{key}.{}", ext_of(name, "m4a")));
                    tokio::fs::rename(&src, &dest).await.or_else(|_| std::fs::copy(&src, &dest).map(|_| ()))?;
                    let _ = state.http.delete(format!("{base}/jobs/{id}")).send().await;
                    return Ok((dest, "download"));
                }
                Some("failed") => anyhow::bail!("download failed: {}", st["message"].as_str().unwrap_or("")),
                _ => {}
            }
        }
        anyhow::bail!("download timed out");
    }
    anyhow::bail!("no way to get this track: not in the library, no host stream, no source link")
}

/// Keeps the track's metadata (and cover when offered) for 48 hours so the listener can look back.
async fn remember(state: &AppState, pubkey: &str, t: &StationTrack) {
    let dir = cache_dir(state).join("meta");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let key = hex::encode(&sha2::Sha256::digest(format!("{pubkey}|{}|{}", t.artist, t.title).as_bytes())[..12]);
    let _ = tokio::fs::write(dir.join(format!("{key}.json")), serde_json::to_vec_pretty(&json!({"host": pubkey, "track": t, "seenAt": crate::db::now_ms()})).unwrap_or_default()).await;
    if let Some(c) = t.cover.as_ref().filter(|c| c.starts_with("http")) {
        let jpg = dir.join(format!("{key}.jpg"));
        if !tokio::fs::try_exists(&jpg).await.unwrap_or(false) {
            if let Ok(r) = state.http.get(c).send().await {
                if let Ok(b) = r.bytes().await {
                    let _ = tokio::fs::write(&jpg, &b).await;
                }
            }
        }
    }
}

async fn evict(state: &AppState) {
    let dir = cache_dir(state);
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let Ok(mut rd) = tokio::fs::read_dir(&d).await else { continue };
        while let Ok(Some(e)) = rd.next_entry().await {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if let Ok(m) = e.metadata().await {
                if m.modified().ok().and_then(|t| t.elapsed().ok()).map(|age| age > KEEP).unwrap_or(false) {
                    let _ = tokio::fs::remove_file(&p).await;
                }
            }
        }
    }
}

pub fn spawn(state: Arc<AppState>, shutdown: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut last_evict = tokio::time::Instant::now();
        loop {
            tokio::select! { _ = shutdown.cancelled() => break, _ = tick.tick() => {} }
            if last_evict.elapsed() > Duration::from_secs(600) {
                evict(&state).await;
                last_evict = tokio::time::Instant::now();
            }
            let Some(pubkey) = state.listen.listening() else { continue };
            let entry = state.stations.feed.lock().unwrap_or_else(|p| p.into_inner()).get(&pubkey).cloned();
            let Some(entry) = entry else {
                state.listen.set("waiting", "no state from this station yet");
                continue;
            };
            let st = entry.state;
            if st.status != station::STATUS_ON_AIR {
                state.listen.set("waiting", format!("station is {}", st.status));
                continue;
            }
            let Some(now) = st.now.clone() else {
                state.listen.set("waiting", "nothing playing");
                continue;
            };
            let same = state.listen.lock().current_key == now.key;
            let expected_ms = st.expected_position_ms(crate::db::now_ms());
            if !same {
                {
                    let mut l = state.listen.lock();
                    l.current_key = now.key.clone();
                    l.now = Some(now.clone());
                    l.status = "resolving".into();
                }
                state.emit(UiEvent::Listen);
                remember(&state, &pubkey, &now).await;
                for n in st.next.iter().take(3) {
                    remember(&state, &pubkey, n).await;
                }
                match resolve(&state, &now).await {
                    Ok((path, via)) => {
                        let item = QueueItem { sha256: String::new(), title: now.title.clone(), artist: now.artist.clone(), album: now.album.clone(), path: path.to_string_lossy().into_owned(), duration_ms: now.duration_ms, link: now.link.clone() };
                        match state.player.load_item(item) {
                            Ok(()) => {
                                let pos = st.expected_position_ms(crate::db::now_ms());
                                state.player.seek(pos as f64 / 1000.0);
                                state.listen.set("playing", format!("via {via}"));
                                info!("listening: {} - {} via {via} at {:.1}s", now.artist, now.title, pos as f64 / 1000.0);
                            }
                            Err(e) => state.listen.set("error", e.to_string()),
                        }
                    }
                    Err(e) => {
                        warn!("listening: {e}");
                        state.listen.set("error", e.to_string());
                    }
                }
                state.emit(UiEvent::Listen);
                continue;
            }
            // Same track: correct drift only when well off.
            let (_, _, pos, playing) = state.player.snapshot();
            let drift = (pos * 1000.0) as i64 - expected_ms;
            state.listen.lock().drift_ms = drift;
            if playing && drift.abs() > DRIFT_MS {
                state.player.seek(expected_ms as f64 / 1000.0);
            }
        }
    });
}

use sha2::Digest;
