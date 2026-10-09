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
    std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).or_else(|| std::env::var("COMPUTERNAME").ok()).or_else(|| std::env::var("HOSTNAME").ok()).unwrap_or_else(|| "CUEd desktop".into())
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
