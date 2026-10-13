//! The desktop player: a queue in front of the C++ two-deck engine.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracing::{info, warn};

use crate::db::Track;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    pub sha256: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(skip)]
    pub path: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
    pub link: Option<String>,
}

impl From<&Track> for QueueItem {
    fn from(t: &Track) -> Self {
        Self { sha256: t.sha256.clone(), title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(), path: t.path.clone(), duration_ms: t.duration_ms, link: t.link.clone() }
    }
}

struct Inner {
    current: Option<QueueItem>,
    queue: Vec<QueueItem>,
    volume: f32,
    /// Bumped on every change the station host should publish.
    generation: u64,
}

pub struct Player {
    engine: Option<cued_audio::Engine>,
    inner: Mutex<Inner>,
    crossfade: Mutex<(f64, u8)>,
    headless: bool,
}

impl Player {
    pub fn new(enable_audio: bool, crossfade_secs: f64, curve: u8) -> Self {
        let engine = if enable_audio {
            match cued_audio::Engine::new(cued_audio::EngineOptions { sample_rate: 48_000, use_device: std::env::var("CUED_AUDIO_NULL").is_err() }) {
                Ok(e) => {
                    info!("audio engine {} ready", cued_audio::version());
                    Some(e)
                }
                Err(e) => {
                    warn!("audio engine unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        // Null output (forced by CUED_AUDIO_NULL, or because no sound device opened): the daemon ticks the clock.
        let headless = engine.as_ref().map(|e| !e.has_device()).unwrap_or(true);
        let p = Self { engine, inner: Mutex::new(Inner { current: None, queue: vec![], volume: 1.0, generation: 0 }), crossfade: Mutex::new((crossfade_secs, curve)), headless };
        p.set_crossfade(crossfade_secs, curve);
        p
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn enabled(&self) -> bool {
        self.engine.is_some()
    }

    pub fn describe(&self) -> serde_json::Value {
        serde_json::json!({ "enabled": self.engine.is_some(), "version": cued_audio::version(), "headless": self.headless })
    }

    pub fn set_crossfade(&self, secs: f64, curve: u8) {
        *self.crossfade.lock().unwrap_or_else(|p| p.into_inner()) = (secs, curve);
        if let Some(e) = &self.engine {
            e.set_crossfade(secs, match curve {
                1 => cued_audio::Curve::Linear,
                2 => cued_audio::Curve::SmoothStep,
                _ => cued_audio::Curve::EqualPower,
            });
        }
    }

    /// Plays `t` now (crossfading from whatever is playing).
    pub fn load(&self, t: &Track) -> anyhow::Result<()> {
        self.load_item(QueueItem::from(t))
    }

    /// Plays any file (a temporary station download, for instance).
    pub fn load_item(&self, item: QueueItem) -> anyhow::Result<()> {
        if let Some(e) = &self.engine {
            e.load(Path::new(&item.path), true)?;
        }
        let mut i = self.lock();
        i.current = Some(item);
        i.generation += 1;
        Ok(())
    }

    pub fn play(&self) {
        if let Some(e) = &self.engine {
            let _ = e.play();
        }
        self.lock().generation += 1;
    }
    pub fn pause(&self) {
        if let Some(e) = &self.engine {
            let _ = e.pause();
        }
        self.lock().generation += 1;
    }
    pub fn stop(&self) {
        if let Some(e) = &self.engine {
            let _ = e.stop();
        }
        let mut i = self.lock();
        i.current = None;
        i.generation += 1;
    }
    pub fn seek(&self, secs: f64) {
        if let Some(e) = &self.engine {
            let _ = e.seek(secs.max(0.0));
        }
        self.lock().generation += 1;
    }
    pub fn set_volume(&self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        if let Some(e) = &self.engine {
            e.set_volume(v);
        }
        self.lock().volume = v;
    }

    pub fn enqueue(&self, t: &Track) {
        self.lock().queue.push(QueueItem::from(t));
        self.lock().generation += 1;
    }
    pub fn clear_queue(&self) {
        let mut i = self.lock();
        i.queue.clear();
        i.generation += 1;
    }
    pub fn queue(&self) -> Vec<QueueItem> {
        self.lock().queue.clone()
    }
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Starts the next queued item (crossfading into it). Returns false when the queue is empty.
    pub fn next(&self) -> bool {
        let next = {
            let mut i = self.lock();
            if i.queue.is_empty() {
                return false;
            }
            i.queue.remove(0)
        };
        if let Some(e) = &self.engine {
            if let Err(err) = e.load(Path::new(&next.path), true) {
                warn!("could not load {}: {err}", next.path);
            }
        }
        let mut i = self.lock();
        i.current = Some(next);
        i.generation += 1;
        true
    }

    pub fn status(&self) -> serde_json::Value {
        let st = self.engine.as_ref().map(|e| e.status());
        let i = self.lock();
        let (secs, curve) = *self.crossfade.lock().unwrap_or_else(|p| p.into_inner());
        serde_json::json!({
            "engine": self.engine.is_some(),
            "playing": st.as_ref().map(|s| s.playing).unwrap_or(false),
            "positionSec": st.as_ref().map(|s| s.position_sec).unwrap_or(0.0),
            "durationSec": st.as_ref().map(|s| s.duration_sec).unwrap_or(0.0),
            "crossfading": st.as_ref().map(|s| s.crossfading).unwrap_or(false),
            "crossfadeProgress": st.as_ref().map(|s| s.crossfade_progress).unwrap_or(0.0),
            "crossfadeSeconds": secs,
            "crossfadeCurve": curve,
            "volume": i.volume,
            "current": i.current,
            "queue": i.queue.len(),
        })
    }

    pub fn spectrum(&self) -> Vec<f32> {
        self.engine.as_ref().map(|e| e.spectrum()).unwrap_or_default()
    }

    /// Headless mode (null device): advance time so the engine behaves in tests and on boxes without sound.
    pub fn tick(&self, frames: u32) {
        if let Some(e) = &self.engine {
            if self.headless {
                e.tick(frames);
            }
        }
    }

    /// Position in the current item, playing flag and the wall clock, for the station host.
    pub fn snapshot(&self) -> (Option<QueueItem>, Vec<QueueItem>, f64, bool) {
        let st = self.engine.as_ref().map(|e| e.status());
        let i = self.lock();
        (i.current.clone(), i.queue.clone(), st.as_ref().map(|s| s.position_sec).unwrap_or(0.0), st.as_ref().map(|s| s.playing).unwrap_or(false))
    }
}

/// Auto-advance: when the current track is about to end and something is queued, crossfade into it.
pub fn spawn_ticker(state: Arc<AppState>, shutdown: tokio_util::sync::CancellationToken) {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = iv.tick() => {}
            }
            state.player.tick(12_000);
            let st = state.player.status();
            let playing = st["playing"].as_bool().unwrap_or(false);
            let pos = st["positionSec"].as_f64().unwrap_or(0.0);
            let dur = st["durationSec"].as_f64().unwrap_or(0.0);
            let fade = st["crossfadeSeconds"].as_f64().unwrap_or(0.0);
            let fading = st["crossfading"].as_bool().unwrap_or(false);
            if playing && !fading && dur > 0.0 && pos >= (dur - fade.max(0.5)) && st["queue"].as_u64().unwrap_or(0) > 0 {
                state.player.next();
            }
        }
    });
}

/// Runs the BPM detector in the background and stores the result.
pub fn analyse_later(state: Arc<AppState>, sha256: String, path: PathBuf) {
    tokio::task::spawn_blocking(move || match cued_audio::analyse(&path) {
        Ok(a) if a.bpm > 0.0 => {
            if let Err(e) = state.db.set_bpm(&sha256, a.bpm as f64) {
                warn!("storing bpm: {e}");
            } else {
                info!("{}: {:.1} BPM (confidence {:.2})", path.file_name().and_then(|f| f.to_str()).unwrap_or("?"), a.bpm, a.confidence);
            }
        }
        Ok(_) => {}
        Err(e) => warn!("analysis of {} failed: {e}", path.display()),
    });
}

pub async fn analyse_now(path: &str) -> anyhow::Result<serde_json::Value> {
    let p = PathBuf::from(path);
    let a = tokio::task::spawn_blocking(move || cued_audio::analyse(&p)).await??;
    Ok(serde_json::json!({ "bpm": a.bpm, "confidence": a.confidence, "firstBeatSec": a.first_beat_sec, "durationSec": a.duration_sec }))
}
