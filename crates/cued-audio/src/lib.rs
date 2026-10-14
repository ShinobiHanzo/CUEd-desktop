//! Safe Rust bindings for the CUEd desktop audio engine (`native/cued-audio`).
//!
//! * [`Engine`]: two decks, a crossfading mixer (equal power / linear /
//!   smooth step, same curves as the phone), device output through miniaudio
//!   or a tick-driven null output for tests and headless use, and a live
//!   64-band spectrum of the mix.
//! * [`analyse`]: whole-file BPM / beat-phase detection with the phone's
//!   `BpmDetector` maths (mono, decimated to ~11 kHz).
//! * [`envelope`]: loudness envelope for a scrubber background.
//!
//! Decoding is miniaudio for WAV / FLAC / MP3 and a background `ffmpeg` pipe
//! for everything else (AAC / M4A, Ogg, ...).
//!
//! Set `CUED_AUDIO_NULL=1` to force the null output regardless of
//! [`EngineOptions::use_device`] (useful in CI).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double, c_float, c_int};
use std::path::Path;
use std::ptr::NonNull;

use serde::{Deserialize, Serialize};

mod ffi {
    use super::*;

    #[repr(C)]
    pub struct cued_engine {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct cued_status {
        pub playing: c_int,
        pub loaded: c_int,
        pub position_sec: c_double,
        pub duration_sec: c_double,
        pub crossfading: c_int,
        pub crossfade_progress: c_float,
        pub path: [c_char; 1024],
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct cued_analysis {
        pub bpm: c_float,
        pub confidence: c_float,
        pub first_beat_sec: c_float,
        pub duration_sec: c_double,
        pub sample_rate: c_int,
        pub channels: c_int,
    }

    extern "C" {
        pub fn cued_audio_version() -> *const c_char;
        pub fn cued_engine_create(sample_rate: c_int, use_device: c_int) -> *mut cued_engine;
        pub fn cued_engine_destroy(e: *mut cued_engine);
        pub fn cued_engine_has_device(e: *mut cued_engine) -> c_int;
        pub fn cued_engine_load(e: *mut cued_engine, path: *const c_char, autoplay: c_int) -> c_int;
        pub fn cued_engine_play(e: *mut cued_engine) -> c_int;
        pub fn cued_engine_pause(e: *mut cued_engine) -> c_int;
        pub fn cued_engine_stop(e: *mut cued_engine) -> c_int;
        pub fn cued_engine_seek(e: *mut cued_engine, seconds: c_double) -> c_int;
        pub fn cued_engine_set_crossfade(e: *mut cued_engine, seconds: c_double, curve: c_int);
        pub fn cued_engine_set_volume(e: *mut cued_engine, v01: c_float);
        pub fn cued_engine_set_karaoke(e: *mut cued_engine, on: c_int, strength: c_float);
        pub fn cued_engine_status(e: *mut cued_engine, out: *mut cued_status);
        pub fn cued_engine_spectrum(e: *mut cued_engine, out: *mut c_float, max_bands: c_int) -> c_int;
        pub fn cued_engine_tick(e: *mut cued_engine, frames: c_int);
        pub fn cued_analyse_file(path: *const c_char, out: *mut cued_analysis) -> c_int;
        pub fn cued_envelope_file(path: *const c_char, out: *mut c_float, columns: c_int) -> c_int;
        pub fn cued_last_error() -> *const c_char;
    }
}

/// Number of spectrum bands the engine produces.
pub const BANDS: usize = 64;

/// Errors from the native engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The native side reported a failure; `message` is `cued_last_error()`.
    #[error("{message} (code {code})")]
    Native { code: i32, message: String },
    /// A path contained a NUL byte or could not be passed to C.
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// `cued_engine_create` returned NULL.
    #[error("could not create engine: {0}")]
    Create(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Crossfade gain curves (the phone's `CrossfadeCurve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[repr(i32)]
pub enum Curve {
    /// Both tracks at -3 dB in the middle; constant perceived loudness.
    #[default]
    EqualPower = 0,
    /// Straight lines; slight dip in the middle.
    Linear = 1,
    /// Outgoing holds then drops late, incoming rises early.
    SmoothStep = 2,
}

/// Options for [`Engine::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// Output sample rate; 0 means 48000.
    pub sample_rate: u32,
    /// `true` opens a real device through miniaudio (falling back to the null
    /// output if none can be opened); `false` uses the tick-driven null output.
    pub use_device: bool,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self { sample_rate: 48000, use_device: true }
    }
}

/// Snapshot of the engine state (the active deck).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Status {
    pub playing: bool,
    pub loaded: bool,
    pub position_sec: f64,
    /// 0 while unknown (an ffmpeg pipe without ffprobe).
    pub duration_sec: f64,
    pub crossfading: bool,
    /// 0..1 during a crossfade, else 0.
    pub crossfade_progress: f32,
    /// Path of the active deck, empty when nothing is loaded.
    pub path: String,
}

/// Result of [`analyse`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Analysis {
    /// 0 when no tempo was found.
    pub bpm: f32,
    /// 0..1.
    pub confidence: f32,
    /// Beat phase: time of the first beat in seconds.
    pub first_beat_sec: f32,
    pub duration_sec: f64,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Library version string, e.g. `0.1.0 (miniaudio 0.11.22)`.
pub fn version() -> &'static str {
    // SAFETY: the C side returns a pointer into a static std::string that
    // lives for the whole process.
    unsafe { CStr::from_ptr(ffi::cued_audio_version()) }
        .to_str()
        .unwrap_or("unknown")
}

fn last_error() -> String {
    // SAFETY: never NULL; points at a thread-local std::string's buffer that
    // is valid until the next native call on this thread, and we copy it now.
    unsafe { CStr::from_ptr(ffi::cued_last_error()) }
        .to_string_lossy()
        .into_owned()
}

fn check(code: c_int) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Native { code, message: last_error() })
    }
}

fn c_path(path: &Path) -> Result<CString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::InvalidPath(path.display().to_string()))
    }
    #[cfg(not(unix))]
    {
        let s = path.to_str().ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
        CString::new(s).map_err(|_| Error::InvalidPath(path.display().to_string()))
    }
}

/// Two-deck crossfading player.
///
/// All methods take `&self`: the native engine serialises its own state with
/// a mutex, so an `Engine` can be shared between threads (for example an
/// `Arc<Engine>` held by both the HTTP API and a status poller).
pub struct Engine {
    ptr: NonNull<ffi::cued_engine>,
}

// SAFETY: every entry point on the C side locks the engine's mutex, and the
// handle is only destroyed in Drop, which needs exclusive ownership.
unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

impl Engine {
    /// Creates an engine. `CUED_AUDIO_NULL=1` in the environment forces the
    /// null output.
    pub fn new(options: EngineOptions) -> Result<Engine> {
        let force_null = std::env::var("CUED_AUDIO_NULL").map(|v| v == "1").unwrap_or(false);
        let use_device = options.use_device && !force_null;
        let sr = c_int::try_from(options.sample_rate).unwrap_or(0);
        // SAFETY: plain FFI call; the returned pointer is owned by us until Drop.
        let raw = unsafe { ffi::cued_engine_create(sr, c_int::from(use_device)) };
        NonNull::new(raw).map(|ptr| Engine { ptr }).ok_or_else(|| Error::Create(last_error()))
    }

    /// Convenience: an engine on the null output (tests, CI, offline rendering).
    pub fn null(sample_rate: u32) -> Result<Engine> {
        Engine::new(EngineOptions { sample_rate, use_device: false })
    }

    fn raw(&self) -> *mut ffi::cued_engine {
        self.ptr.as_ptr()
    }

    /// `true` when a real audio device drives the mixer; `false` for the null
    /// output, which only advances on [`Engine::tick`].
    pub fn has_device(&self) -> bool {
        // SAFETY: valid handle.
        unsafe { ffi::cued_engine_has_device(self.raw()) != 0 }
    }

    /// Loads `path` into the idle deck. While something is playing this starts
    /// a crossfade of the configured length; otherwise the track becomes the
    /// active deck and plays at once when `autoplay` is set.
    pub fn load(&self, path: impl AsRef<Path>, autoplay: bool) -> Result<()> {
        let c = c_path(path.as_ref())?;
        // SAFETY: valid handle and NUL-terminated path.
        check(unsafe { ffi::cued_engine_load(self.raw(), c.as_ptr(), c_int::from(autoplay)) })
    }

    pub fn play(&self) -> Result<()> {
        // SAFETY: valid handle.
        check(unsafe { ffi::cued_engine_play(self.raw()) })
    }

    pub fn pause(&self) -> Result<()> {
        // SAFETY: valid handle.
        check(unsafe { ffi::cued_engine_pause(self.raw()) })
    }

    /// Pauses and rewinds to 0 (completing any crossfade first).
    pub fn stop(&self) -> Result<()> {
        // SAFETY: valid handle.
        check(unsafe { ffi::cued_engine_stop(self.raw()) })
    }

    pub fn seek(&self, seconds: f64) -> Result<()> {
        // SAFETY: valid handle.
        check(unsafe { ffi::cued_engine_seek(self.raw(), seconds) })
    }

    /// Crossfade length (clamped to >= 0; 0 switches tracks at once) and curve.
    pub fn set_crossfade(&self, seconds: f64, curve: Curve) {
        // SAFETY: valid handle.
        unsafe { ffi::cued_engine_set_crossfade(self.raw(), seconds, curve as c_int) }
    }

    /// Master volume in 0..1 (clamped).
    pub fn set_volume(&self, v01: f32) {
        // SAFETY: valid handle.
        unsafe { ffi::cued_engine_set_volume(self.raw(), v01) }
    }

    /// Karaoke: cancels the centre of the mix (the lead vocal) while keeping its bass; `strength` 0..1.
    pub fn set_karaoke(&self, on: bool, strength: f32) {
        // SAFETY: valid handle.
        unsafe { ffi::cued_engine_set_karaoke(self.raw(), on as c_int, strength) }
    }

    pub fn status(&self) -> Status {
        let mut st = ffi::cued_status {
            playing: 0,
            loaded: 0,
            position_sec: 0.0,
            duration_sec: 0.0,
            crossfading: 0,
            crossfade_progress: 0.0,
            path: [0; 1024],
        };
        // SAFETY: valid handle and out pointer; the C side NUL-terminates path.
        unsafe { ffi::cued_engine_status(self.raw(), &mut st) };
        let path = {
            let bytes: Vec<u8> = st.path.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Status {
            playing: st.playing != 0,
            loaded: st.loaded != 0,
            position_sec: st.position_sec,
            duration_sec: st.duration_sec,
            crossfading: st.crossfading != 0,
            crossfade_progress: st.crossfade_progress,
            path,
        }
    }

    /// Latest [`BANDS`]-band spectrum (0..1) of the mix being output.
    pub fn spectrum(&self) -> Vec<f32> {
        let mut out = vec![0.0f32; BANDS];
        // SAFETY: valid handle; out has BANDS elements.
        let n = unsafe { ffi::cued_engine_spectrum(self.raw(), out.as_mut_ptr(), BANDS as c_int) };
        out.truncate(usize::try_from(n).unwrap_or(0));
        out
    }

    /// Null output only: renders `frames` frames through the mixer, advancing
    /// positions and crossfades. Ignored when a real device is active.
    pub fn tick(&self, frames: u32) {
        let n = c_int::try_from(frames).unwrap_or(c_int::MAX);
        // SAFETY: valid handle.
        unsafe { ffi::cued_engine_tick(self.raw(), n) }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: we own the handle and it is destroyed exactly once.
        unsafe { ffi::cued_engine_destroy(self.raw()) }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").field("has_device", &self.has_device()).field("status", &self.status()).finish()
    }
}

/// Decodes the whole file (mono, decimated to ~11 kHz like the phone, capped
/// at 8 minutes) and runs the BPM detector.
pub fn analyse(path: impl AsRef<Path>) -> Result<Analysis> {
    let c = c_path(path.as_ref())?;
    let mut out = ffi::cued_analysis::default();
    // SAFETY: NUL-terminated path and a valid out pointer.
    check(unsafe { ffi::cued_analyse_file(c.as_ptr(), &mut out) })?;
    Ok(Analysis {
        bpm: out.bpm,
        confidence: out.confidence,
        first_beat_sec: out.first_beat_sec,
        duration_sec: out.duration_sec,
        sample_rate: u32::try_from(out.sample_rate).unwrap_or(0),
        channels: u32::try_from(out.channels).unwrap_or(0),
    })
}

/// Loudness envelope with `columns` values in 0..1 for a scrubber background.
pub fn envelope(path: impl AsRef<Path>, columns: usize) -> Result<Vec<f32>> {
    let c = c_path(path.as_ref())?;
    let n = c_int::try_from(columns.max(1)).map_err(|_| Error::InvalidPath("too many columns".into()))?;
    let mut out = vec![0.0f32; columns.max(1)];
    // SAFETY: NUL-terminated path; out holds `columns` floats.
    check(unsafe { ffi::cued_envelope_file(c.as_ptr(), out.as_mut_ptr(), n) })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("cued-audio-rs-{}-{}-{}", std::process::id(), n, name))
    }

    /// Writes a 16-bit stereo RIFF/WAVE file from mono samples.
    fn write_wav(path: &Path, mono: &[f32], sr: u32) {
        let channels: u16 = 2;
        let data_bytes = (mono.len() * channels as usize * 2) as u32;
        let mut f = std::fs::File::create(path).expect("create wav");
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&channels.to_le_bytes());
        h.extend_from_slice(&sr.to_le_bytes());
        h.extend_from_slice(&(sr * channels as u32 * 2).to_le_bytes());
        h.extend_from_slice(&(channels * 2).to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data_bytes.to_le_bytes());
        f.write_all(&h).unwrap();
        let mut body = Vec::with_capacity(data_bytes as usize);
        for &s in mono {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            for _ in 0..channels {
                body.extend_from_slice(&v.to_le_bytes());
            }
        }
        f.write_all(&body).unwrap();
    }

    /// Same click track as the phone's BpmDetectorTest.
    fn click(bpm: f32, seconds: f32, sr: u32) -> Vec<f32> {
        let n = (seconds * sr as f32) as usize;
        let mut pcm = vec![0.0f32; n];
        let period = 60.0 / bpm * sr as f32;
        let mut t = 0.0f32;
        while (t as usize) < n {
            let start = t as usize;
            for i in 0..2000 {
                let idx = start + i;
                if idx >= n {
                    break;
                }
                pcm[idx] += ((-(i as f64) / 300.0).exp() * (i as f64 * 0.3).sin()) as f32;
            }
            t += period;
        }
        pcm
    }

    fn tone(freq: f64, seconds: f32, sr: u32) -> Vec<f32> {
        (0..(seconds * sr as f32) as usize)
            .map(|i| (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / sr as f64).sin()) as f32)
            .collect()
    }

    #[test]
    fn version_mentions_miniaudio() {
        assert!(version().contains("miniaudio"), "{}", version());
    }

    #[test]
    fn analyse_click_track_finds_120_bpm() {
        let wav = temp_path("click120.wav");
        write_wav(&wav, &click(120.0, 20.0, 44100), 44100);
        let a = analyse(&wav).expect("analyse");
        let octave_ok = (a.bpm - 120.0).abs() <= 2.0 || (a.bpm - 240.0).abs() <= 4.0 || (a.bpm - 60.0).abs() <= 1.0;
        assert!(octave_ok, "bpm {}", a.bpm);
        assert!((118.0..=122.0).contains(&a.bpm), "bpm {}", a.bpm);
        assert!(a.confidence > 0.2, "confidence {}", a.confidence);
        assert!((a.duration_sec - 20.0).abs() < 0.01, "duration {}", a.duration_sec);
        assert_eq!(a.sample_rate, 44100);
        assert_eq!(a.channels, 2);
        assert!(a.first_beat_sec.abs() < 0.05, "first beat {}", a.first_beat_sec);
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"bpm\""));

        let env = envelope(&wav, 64).expect("envelope");
        assert_eq!(env.len(), 64);
        assert!(env.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(env.iter().any(|&v| v > 0.0));
        let _ = std::fs::remove_file(&wav);
    }

    #[test]
    fn analyse_missing_file_is_an_error() {
        let err = analyse(temp_path("missing.wav")).unwrap_err();
        match err {
            Error::Native { code, message } => {
                assert!(code < 0);
                assert!(message.contains("missing"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(analyse("a\0b"), Err(Error::InvalidPath(_))));
    }

    #[test]
    fn engine_plays_ticks_and_crossfades_on_the_null_device() {
        let sr = 48000u32;
        let a = temp_path("a.wav");
        let b = temp_path("b.wav");
        write_wav(&a, &tone(220.0, 4.0, sr), sr);
        write_wav(&b, &tone(3000.0, 4.0, sr), sr);

        let engine = Engine::new(EngineOptions { sample_rate: sr, use_device: false }).expect("engine");
        assert!(!engine.has_device());
        let st = engine.status();
        assert!(!st.loaded && !st.playing && st.path.is_empty());
        assert!(engine.play().is_err());

        engine.load(&a, true).expect("load a");
        let st = engine.status();
        assert!(st.loaded && st.playing && !st.crossfading);
        assert_eq!(Path::new(&st.path), a.as_path());
        assert!((st.duration_sec - 4.0).abs() < 1e-6);

        engine.tick(sr / 2);
        let st = engine.status();
        assert!((st.position_sec - 0.5).abs() < 1e-9, "position {}", st.position_sec);

        let spec = engine.spectrum();
        assert_eq!(spec.len(), BANDS);
        let peak = spec.iter().enumerate().fold(0, |p, (i, &v)| if v > spec[p] { i } else { p });
        assert!(peak < 32, "peak band {peak}");

        engine.pause().unwrap();
        engine.tick(sr);
        assert!((engine.status().position_sec - 0.5).abs() < 1e-9);
        engine.play().unwrap();
        engine.seek(1.0).unwrap();
        assert!((engine.status().position_sec - 1.0).abs() < 1e-9);
        assert!(engine.seek(-1.0).is_err());

        engine.set_crossfade(1.0, Curve::EqualPower);
        engine.load(&b, true).expect("load b");
        let st = engine.status();
        assert!(st.crossfading);
        assert_eq!(Path::new(&st.path), a.as_path());
        engine.tick(sr / 2);
        let st = engine.status();
        assert!(st.crossfading);
        assert!((st.crossfade_progress - 0.5).abs() < 1e-5, "progress {}", st.crossfade_progress);
        engine.tick(sr / 2);
        let st = engine.status();
        assert!(!st.crossfading);
        assert_eq!(Path::new(&st.path), b.as_path());
        assert!(st.playing);
        assert!((st.position_sec - 1.0).abs() < 1e-9, "position {}", st.position_sec);

        engine.tick(4096);
        let spec = engine.spectrum();
        let peak = spec.iter().enumerate().fold(0, |p, (i, &v)| if v > spec[p] { i } else { p });
        assert!(peak > 32, "peak band {peak}");

        engine.set_volume(0.5);
        engine.stop().unwrap();
        let st = engine.status();
        assert!(!st.playing && st.loaded && st.position_sec == 0.0);

        assert!(engine.load(temp_path("nope.wav"), true).is_err());
        assert_eq!(Path::new(&engine.status().path), b.as_path());

        let json = serde_json::to_value(engine.status()).unwrap();
        assert_eq!(json["loaded"], true);

        drop(engine);
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
    }

    #[test]
    fn null_env_forces_null_output() {
        // Serialise with the other engine tests through the env var: set, create, unset.
        std::env::set_var("CUED_AUDIO_NULL", "1");
        let engine = Engine::new(EngineOptions { sample_rate: 44100, use_device: true }).expect("engine");
        std::env::remove_var("CUED_AUDIO_NULL");
        assert!(!engine.has_device());
    }
}
