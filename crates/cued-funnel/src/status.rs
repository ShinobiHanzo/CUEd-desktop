//! Counters the daemon shows in its UI.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Live state of the funnel client. Share it as `Arc<FunnelStatus>`.
#[derive(Debug, Default)]
pub struct FunnelStatus {
    pub connected: AtomicBool,
    pub streams_open: AtomicU64,
    pub streams_total: AtomicU64,
    /// Bytes received from the edge (public clients → daemon).
    pub bytes_in: AtomicU64,
    /// Bytes sent to the edge (daemon → public clients).
    pub bytes_out: AtomicU64,
    pub last_error: Mutex<Option<String>>,
}

/// A plain copy of [`FunnelStatus`], for JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FunnelStatusSnapshot {
    pub connected: bool,
    pub streams_open: u64,
    pub streams_total: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub last_error: Option<String>,
}

impl FunnelStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn set_connected(&self, on: bool) {
        self.connected.store(on, Ordering::Relaxed);
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.into());
    }

    pub fn clear_error(&self) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn snapshot(&self) -> FunnelStatusSnapshot {
        FunnelStatusSnapshot {
            connected: self.connected.load(Ordering::Relaxed),
            streams_open: self.streams_open.load(Ordering::Relaxed),
            streams_total: self.streams_total.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            last_error: self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serialises() {
        let s = FunnelStatus::new();
        s.set_connected(true);
        s.streams_total.fetch_add(3, Ordering::Relaxed);
        s.set_error("boom");
        let snap = s.snapshot();
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["connected"], true);
        assert_eq!(json["streams_total"], 3);
        assert_eq!(json["last_error"], "boom");
        s.clear_error();
        assert_eq!(s.snapshot().last_error, None);
    }
}
