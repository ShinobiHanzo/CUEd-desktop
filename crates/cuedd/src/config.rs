//! Settings a person changes in the UI, kept as one JSON file in the data dir.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name shown on the phone when pairing.
    pub name: String,
    /// Public relays the desktop mirrors to and reads from. Any `wss://` works, including your own.
    pub upstream_relays: Vec<String>,
    /// Re-publish events accepted from paired devices to the upstream relays.
    pub mirror_out: bool,
    /// Pull followed stations from upstream relays into the embedded relay.
    pub mirror_in: bool,
    pub crossfade_seconds: f64,
    /// 0 equal power, 1 linear, 2 smooth step (same numbering as the phone).
    pub crossfade_curve: u8,
    /// Output format the companion asks spotdl for.
    pub companion_format: String,
    /// Name of the desktop's own station.
    pub station_name: String,
    /// Let devices on the LAN that are not paired use the companion downloader (pre-0.2.4 behaviour).
    pub companion_open_lan: bool,
    /// Karaoke: cancel the centre (lead vocal) of what plays, keeping its bass.
    pub karaoke: bool,
    /// 0..1, how much of the centre to remove.
    pub karaoke_strength: f32,
    /// Host a shared queue: friends on the Wi-Fi may search and queue (`docs/protocol.md` §13).
    pub party_enabled: bool,
    /// Guests may also skip, pause and resume, not only queue.
    pub party_controls: bool,
    /// Keep the listening ledger (plays table). Off stops recording; existing rows stay until cleared.
    pub ledger: bool,
    /// Download the next release in the background and install it on the next launch.
    pub auto_update: bool,
    /// The version whose patch notes have been read, so the "what's new" card shows once.
    pub notes_seen_version: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: hostname(),
            upstream_relays: vec!["wss://relay.damus.io".into(), "wss://nos.lol".into(), "wss://relay.primal.net".into()],
            mirror_out: true,
            mirror_in: true,
            crossfade_seconds: 6.0,
            crossfade_curve: 0,
            companion_format: "m4a".into(),
            station_name: String::new(),
            companion_open_lan: false,
            karaoke: false,
            karaoke_strength: 1.0,
            party_enabled: false,
            party_controls: true,
            ledger: true,
            auto_update: true,
            notes_seen_version: String::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

pub fn hostname() -> String {
    let clean = |s: String| {
        let s = s.trim().trim_end_matches(".local").trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .and_then(clean)
        .or_else(|| std::env::var("COMPUTERNAME").ok().and_then(clean))
        .or_else(|| std::env::var("HOSTNAME").ok().and_then(clean))
        .or_else(|| {
            // macOS and the BSDs have no /etc/hostname; `hostname` is everywhere.
            std::process::Command::new("hostname").output().ok().filter(|o| o.status.success()).and_then(|o| clean(String::from_utf8_lossy(&o.stdout).into_owned()))
        })
        .unwrap_or_else(|| "CUEd desktop".into())
}

/// `$XDG_DATA_HOME/cued` or `~/.local/share/cued`, `%APPDATA%\cued` on Windows.
pub fn default_data_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(x).join("cued");
    }
    if let Ok(a) = std::env::var("APPDATA") {
        return PathBuf::from(a).join("cued");
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local").join("share").join("cued")
}
