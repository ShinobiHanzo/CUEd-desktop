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
    /// Wall clock when `current` started, for the ledger.
    started_at: i64,
    /// Set once the current item has been written to the ledger outbox (end of track).
    closed: bool,
}

/// A finished (or abandoned) play, waiting to be written to the ledger.
#[derive(Debug, Clone)]
pub struct PlayRecord {
    pub sha256: String,
    pub started_at: i64,
    pub listened_ms: i64,
    pub completed: bool,
}

pub struct Player {
    engine: Option<cued_audio::Engine>,
    inner: Mutex<Inner>,
    crossfade: Mutex<(f64, u8)>,
    karaoke: Mutex<(bool, f32)>,
    headless: bool,
    /// Plays to record; drained by the ticker so the player never touches the database itself.
    outbox: Mutex<Vec<PlayRecord>>,
}

/// A play counts once it ran 30 s or half the track, whichever comes first; it is "completed" past 90 %.
const MIN_LISTEN_SEC: f64 = 30.0;

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
        let p = Self { engine, inner: Mutex::new(Inner { current: None, queue: vec![], volume: 1.0, generation: 0, started_at: 0, closed: false }), crossfade: Mutex::new((crossfade_secs, curve)), karaoke: Mutex::new((false, 1.0)), headless, outbox: Mutex::new(Vec::new()) };
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
        let pos = self.position_sec();
        if let Some(e) = &self.engine {
            e.load(Path::new(&item.path), true)?;
        }
        let mut i = self.lock();
        self.close_current(&mut i, pos);
        i.current = Some(item);
        i.started_at = crate::db::now_ms();
        i.closed = false;
        i.generation += 1;
        Ok(())
    }

    fn position_sec(&self) -> f64 {
        self.engine.as_ref().map(|e| e.status().position_sec).unwrap_or(0.0)
    }

    /// Writes the current item to the ledger outbox (once) if it played long enough. Caller holds `inner`.
    fn close_current(&self, i: &mut Inner, position_sec: f64) {
        if i.closed {
            return;
        }
        let Some(cur) = &i.current else { return };
        i.closed = true;
        let dur = cur.duration_ms as f64 / 1000.0;
        let listened = if position_sec > 0.0 { position_sec } else { (crate::db::now_ms() - i.started_at) as f64 / 1000.0 };
        let counts = listened >= MIN_LISTEN_SEC || (dur > 0.0 && listened >= dur * 0.5);
        if !counts || cur.sha256.is_empty() {
            return;
        }
        let completed = dur > 0.0 && listened >= dur * 0.9;
        self.outbox.lock().unwrap_or_else(|p| p.into_inner()).push(PlayRecord { sha256: cur.sha256.clone(), started_at: i.started_at, listened_ms: (listened * 1000.0) as i64, completed });
    }

    /// The current item reached its end on its own (nothing queued): count it as completed.
    pub fn mark_ended(&self) {
        let pos = self.position_sec();
        let mut i = self.lock();
        self.close_current(&mut i, pos);
    }

    /// Plays recorded since the last call.
    pub fn take_plays(&self) -> Vec<PlayRecord> {
        std::mem::take(&mut *self.outbox.lock().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn set_karaoke(&self, on: bool, strength: f32) {
        let strength = if strength.is_nan() { 1.0 } else { strength.clamp(0.0, 1.0) };
        if let Some(e) = &self.engine {
            e.set_karaoke(on, strength);
        }
        *self.karaoke.lock().unwrap_or_else(|p| p.into_inner()) = (on, strength);
    }

    pub fn karaoke(&self) -> (bool, f32) {
        *self.karaoke.lock().unwrap_or_else(|p| p.into_inner())
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
        let pos = self.position_sec();
        if let Some(e) = &self.engine {
            let _ = e.stop();
        }
        let mut i = self.lock();
        self.close_current(&mut i, pos);
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
        let pos = self.position_sec();
        if let Some(e) = &self.engine {
            if let Err(err) = e.load(Path::new(&next.path), true) {
                warn!("could not load {}: {err}", next.path);
            }
        }
        let mut i = self.lock();
        self.close_current(&mut i, pos);
        i.current = Some(next);
        i.started_at = crate::db::now_ms();
        i.closed = false;
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
            "karaoke": self.karaoke().0,
            "karaokeStrength": self.karaoke().1,
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
            // Ledger: a track that ran out with nothing queued, and anything the player closed since the last tick.
            if !playing && dur > 0.0 && pos >= dur - 0.75 && st["current"].is_object() {
                state.player.mark_ended();
            }
            if state.settings().ledger {
                for p in state.player.take_plays() {
                    if let Err(e) = state.db.record_play(&p.sha256, p.started_at, p.listened_ms, p.completed) {
                        warn!("ledger: {e}");
                    }
                }
            } else {
                state.player.take_plays();
            }
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
