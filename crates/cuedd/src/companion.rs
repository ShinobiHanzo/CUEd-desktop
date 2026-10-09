//! Runs the Python download worker (`python/cued_companion`) next to the
//! daemon, keeps it alive, and moves what it downloads into the library.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::state::{AppState, UiEvent};

#[derive(Debug, Clone, Serialize, Default)]
pub struct CompanionInfo {
    pub running: bool,
    pub port: u16,
    pub python: String,
    pub module_dir: Option<String>,
    pub error: Option<String>,
    pub imported: u64,
}

fn python() -> String {
    std::env::var("CUED_PYTHON").unwrap_or_else(|_| if cfg!(windows) { "python".into() } else { "python3".into() })
}

/// Finds the `python/` directory holding `cued_companion`, next to the source tree or the installed binary.
fn module_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CUED_COMPANION_DIR") {
        return Some(PathBuf::from(d));
    }
    let mut candidates = vec![PathBuf::from("python")];
    if let Ok(exe) = std::env::current_exe() {
        let mut p = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            if let Some(dir) = p.clone() {
                candidates.push(dir.join("python"));
                candidates.push(dir.join("share").join("cued").join("python"));
                p = dir.parent().map(|x| x.to_path_buf());
            }
        }
    }
    candidates.into_iter().find(|c| c.join("cued_companion").join("server.py").is_file())
}

async fn free_port() -> u16 {
    tokio::net::TcpListener::bind("127.0.0.1:0").await.and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(8766)
}

pub fn spawn(state: Arc<AppState>, shutdown: CancellationToken) {
    tokio::spawn(run(state, shutdown));
}

async fn run(state: Arc<AppState>, shutdown: CancellationToken) {
    let py = python();
    let Some(dir) = module_dir() else {
        warn!("companion: python/cued_companion not found; downloads via the phone's Companion backend are off (set CUED_COMPANION_DIR)");
        *state.companion.lock().unwrap() = Some(CompanionInfo { python: py, error: Some("cued_companion package not found".into()), ..Default::default() });
        return;
    };
    let downloads = state.store.downloads_dir();
    let mut backoff = 1u64;
    let mut imported_jobs: HashSet<String> = HashSet::new();
    let mut imported_count = 0u64;
    while !shutdown.is_cancelled() {
        let port = free_port().await;
        let mut cmd = tokio::process::Command::new(&py);
        cmd.args(["-m", "cued_companion.server", "--bind", "127.0.0.1", "--port", &port.to_string(), "--dir"])
            .arg(&downloads)
            .env("PYTHONPATH", &dir)
            .env("PYTHONUNBUFFERED", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!("companion: cannot start {py}: {e}");
                *state.companion.lock().unwrap() = Some(CompanionInfo { python: py.clone(), module_dir: Some(dir.display().to_string()), error: Some(format!("cannot start {py}: {e}")), ..Default::default() });
                return;
            }
        };
        let stderr = child.stderr.take();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            if let Some(err) = stderr {
                let mut lines = tokio::io::BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!("companion: {l}");
                }
            }
        });
        // Wait for /health.
        let base = format!("http://127.0.0.1:{port}");
        let mut healthy = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if state.http.get(format!("{base}/health")).send().await.map(|r| r.status().is_success()).unwrap_or(false) {
                healthy = true;
                break;
            }
        }
        if !healthy {
            let _ = child.kill().await;
            let msg = "worker did not answer /health (is spotdl installed for this python? `python3 -m pip install spotdl`)";
            warn!("companion: {msg}");
            *state.companion.lock().unwrap() = Some(CompanionInfo { python: py.clone(), module_dir: Some(dir.display().to_string()), error: Some(msg.into()), ..Default::default() });
            tokio::select! { _ = shutdown.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(backoff.min(60))) => {} }
            backoff = (backoff * 2).min(60);
            continue;
        }
        backoff = 1;
        info!("companion worker on {base} (downloads → {})", downloads.display());
        *state.companion.lock().unwrap() = Some(CompanionInfo { running: true, port, python: py.clone(), module_dir: Some(dir.display().to_string()), error: None, imported: imported_count });

        // Poll jobs, import finished ones, until the child dies or we shut down.
        let mut iv = tokio::time::interval(Duration::from_secs(3));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => { let _ = child.kill().await; return; }
                status = child.wait() => { warn!("companion exited: {status:?}"); break; }
                _ = iv.tick() => {
                    if let Ok(resp) = state.http.get(format!("{base}/jobs")).send().await {
                        if let Ok(jobs) = resp.json::<serde_json::Value>().await {
                            let list: Vec<serde_json::Value> = jobs.as_array().cloned().or_else(|| jobs["jobs"].as_array().cloned()).unwrap_or_default();
                            for job in list {
                                let id = job["id"].as_str().unwrap_or("").to_string();
                                if id.is_empty() || job["status"].as_str() != Some("done") || imported_jobs.contains(&id) || state.listen.is_temp_job(&id) { continue; }
                                imported_jobs.insert(id.clone());
                                let files: Vec<String> = job["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect()).unwrap_or_default();
                                for name in files.iter().filter(|n| !n.to_ascii_lowercase().ends_with(".lrc") && !n.to_ascii_lowercase().ends_with(".txt")) {
                                    let path = downloads.join(crate::store::safe_segment(name));
                                    if !path.is_file() { continue; }
                                    match crate::api::library::import_file(&state, path, None, vec![], None, job["url"].as_str().map(str::to_string), "companion").await {
                                        Ok(t) => { imported_count += 1; state.emit(UiEvent::Companion { job: id.clone(), status: "imported".into(), message: format!("{} - {}", t.artist, t.title) }); }
                                        Err(e) => warn!("companion: importing {name}: {}", e.1),
                                    }
                                }
                                if let Some(c) = state.companion.lock().unwrap().as_mut() { c.imported = imported_count; }
                            }
                        }
                    }
                }
            }
        }
        if let Some(c) = state.companion.lock().unwrap().as_mut() {
            c.running = false;
        }
        tokio::select! { _ = shutdown.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
    }
}
