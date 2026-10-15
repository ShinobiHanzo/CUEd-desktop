//! Self-update, the self-custody way: the daemon looks at GitHub Releases for
//! its own target, downloads the next archive in the background, checks it
//! against the published SHA-256 and keeps it staged under `<data>/updates/`.
//! On the next launch (or on "Install now") the staged binaries are swapped in
//! and the daemon re-executes itself; the new version then shows what changed,
//! from the changelog compiled into the binary, until the notes are dismissed.
//! Nothing is installed behind your back mid-session, and `auto_update` turns
//! even the download off.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::state::AppState;

pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const TARGET: &str = env!("CUED_TARGET");
const REPO: &str = "ShinobiHanzo/CUEd-desktop";

/// `CUED_UPDATE_API` overrides the releases API base (tests, mirrors, an air-gapped LAN copy).
fn api_base() -> String {
    std::env::var("CUED_UPDATE_API").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "https://api.github.com".into())
}
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(45);

/// A release found on GitHub that fits this build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Latest {
    pub version: String,
    pub tag: String,
    #[serde(rename = "archiveUrl")]
    pub archive_url: String,
    #[serde(rename = "archiveName")]
    pub archive_name: String,
    #[serde(rename = "shaUrl")]
    pub sha_url: Option<String>,
    pub size: u64,
    #[serde(rename = "publishedAt")]
    pub published_at: String,
    pub notes: String,
}

/// What `updates/pending/meta.json` holds: a verified archive waiting for the next launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Staged {
    pub version: String,
    pub target: String,
    pub archive: String,
    pub sha256: String,
    #[serde(rename = "stagedAt")]
    pub staged_at: i64,
}

/// `updates/applied.json`: written just before the swap, read by the new version, kept until the notes are seen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Applied {
    pub from: String,
    pub to: String,
    #[serde(rename = "appliedAt")]
    pub applied_at: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateInfo {
    pub latest: Option<Latest>,
    pub staged: Option<Staged>,
    /// `idle`, `checking`, `downloading`, `staged`, `up-to-date`, `error`.
    pub state: String,
    pub progress: f32,
    pub error: Option<String>,
    #[serde(rename = "checkedAt")]
    pub checked_at: Option<i64>,
    #[serde(rename = "applyError")]
    pub apply_error: Option<String>,
}

// ---- versions and notes ------------------------------------------------------------------------

pub fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split('-').next().unwrap_or("").split('.').map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)).collect()
}

pub fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let (pa, pb) = (parse_version(a), parse_version(b));
    for i in 0..pa.len().max(pb.len()) {
        let o = pa.get(i).copied().unwrap_or(0).cmp(&pb.get(i).copied().unwrap_or(0));
        if o != std::cmp::Ordering::Equal {
            return o;
        }
    }
    std::cmp::Ordering::Equal
}

/// `(version, body)` for every `## x.y.z` section of a changelog, newest first.
pub fn sections(changelog: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in changelog.lines() {
        if let Some(v) = line.strip_prefix("## ") {
            out.push((v.trim().to_string(), String::new()));
        } else if let Some(cur) = out.last_mut() {
            cur.1.push_str(line);
            cur.1.push('\n');
        }
    }
    for s in &mut out {
        s.1 = s.1.trim().to_string();
    }
    out
}

/// The sections newer than `from` and not newer than `to`, newest first, as one markdown string.
pub fn notes_between(changelog: &str, from: Option<&str>, to: &str) -> String {
    sections(changelog)
        .into_iter()
        .filter(|(v, _)| cmp_versions(v, to) != std::cmp::Ordering::Greater && from.map(|f| cmp_versions(v, f) == std::cmp::Ordering::Greater).unwrap_or(true))
        .map(|(v, body)| format!("## {v}\n\n{body}\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- paths -------------------------------------------------------------------------------------

fn updates_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("updates")
}
fn pending_dir(data_dir: &Path) -> PathBuf {
    updates_dir(data_dir).join("pending")
}
fn applied_path(data_dir: &Path) -> PathBuf {
    updates_dir(data_dir).join("applied.json")
}
fn apply_error_path(data_dir: &Path) -> PathBuf {
    updates_dir(data_dir).join("apply-error.txt")
}

pub fn read_staged(data_dir: &Path) -> Option<Staged> {
    let meta: Staged = serde_json::from_slice(&std::fs::read(pending_dir(data_dir).join("meta.json")).ok()?).ok()?;
    if !pending_dir(data_dir).join(&meta.archive).is_file() {
        return None;
    }
    Some(meta)
}

/// The record a successful swap left for us; dropped when it names another version than the one running.
pub fn read_applied(data_dir: &Path) -> Option<Applied> {
    let a: Applied = serde_json::from_slice(&std::fs::read(applied_path(data_dir)).ok()?).ok()?;
    if a.to != CURRENT {
        let _ = std::fs::remove_file(applied_path(data_dir));
        return None;
    }
    Some(a)
}

pub fn read_apply_error(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(apply_error_path(data_dir)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn forget_applied(data_dir: &Path) {
    let _ = std::fs::remove_file(applied_path(data_dir));
}

pub fn discard_staged(data_dir: &Path) {
    let _ = std::fs::remove_dir_all(pending_dir(data_dir));
}

/// Whether the swap can work at all: the directory holding the executable must be writable.
pub fn can_install() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let Some(dir) = exe.parent() else { return false };
    let probe = dir.join(".cued-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---- checking and staging ----------------------------------------------------------------------

/// Picks this build's archive out of a GitHub release object.
pub fn pick_asset(release: &serde_json::Value, target: &str) -> Option<Latest> {
    let tag = release.get("tag_name")?.as_str()?.to_string();
    if release.get("draft").and_then(|d| d.as_bool()).unwrap_or(false) {
        return None;
    }
    let ext = if target.contains("windows") { "zip" } else { "tar.gz" };
    let want = format!("cued-desktop-{tag}-{target}.{ext}");
    let assets = release.get("assets")?.as_array()?;
    let archive = assets.iter().find(|a| a.get("name").and_then(|n| n.as_str()) == Some(want.as_str()))?;
    let sha = assets.iter().find(|a| a.get("name").and_then(|n| n.as_str()) == Some(format!("{want}.sha256").as_str()));
    Some(Latest {
        version: tag.trim_start_matches('v').to_string(),
        tag: tag.clone(),
        archive_url: archive.get("browser_download_url")?.as_str()?.to_string(),
        archive_name: want,
        sha_url: sha.and_then(|s| s.get("browser_download_url")).and_then(|u| u.as_str()).map(str::to_string),
        size: archive.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
        published_at: release.get("published_at").and_then(|p| p.as_str()).unwrap_or("").to_string(),
        notes: release.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string(),
    })
}

fn set_state(s: &AppState, f: impl FnOnce(&mut UpdateInfo)) {
    let mut u = s.update.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut u);
}

/// One check against GitHub; downloads and stages when something newer exists and `stage` is set.
pub async fn check(s: &Arc<AppState>, stage: bool) -> anyhow::Result<Option<Latest>> {
    set_state(s, |u| {
        u.state = "checking".into();
        u.error = None;
    });
    let result = async {
        let url = format!("{}/repos/{REPO}/releases/latest", api_base());
        let resp = s.http.get(&url).header("User-Agent", format!("cuedd/{CURRENT} (+https://github.com/{REPO})")).header("Accept", "application/vnd.github+json").send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("GitHub answered {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await?;
        Ok::<_, anyhow::Error>(pick_asset(&v, TARGET))
    }
    .await;
    let now = crate::db::now_ms();
    let latest = match result {
        Ok(l) => l,
        Err(e) => {
            set_state(s, |u| {
                u.state = "error".into();
                u.error = Some(e.to_string());
                u.checked_at = Some(now);
            });
            return Err(e);
        }
    };
    let newer = latest.as_ref().filter(|l| cmp_versions(&l.version, CURRENT) == std::cmp::Ordering::Greater).cloned();
    let already = read_staged(&s.data_dir).filter(|st| newer.as_ref().map(|l| l.version == st.version).unwrap_or(false));
    set_state(s, |u| {
        u.latest = latest.clone();
        u.checked_at = Some(now);
        u.staged = read_staged(&s.data_dir);
        u.state = if newer.is_none() { "up-to-date".into() } else if already.is_some() { "staged".into() } else { "available".into() };
    });
    if let (Some(l), true, None) = (&newer, stage, already) {
        if let Err(e) = download_and_stage(s, l).await {
            warn!("update: staging {} failed: {e}", l.version);
            set_state(s, |u| {
                u.state = "error".into();
                u.error = Some(e.to_string());
            });
        }
    }
    Ok(latest)
}

async fn download_and_stage(s: &Arc<AppState>, l: &Latest) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    let dir = pending_dir(&s.data_dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    set_state(s, |u| {
        u.state = "downloading".into();
        u.progress = 0.0;
    });
    let expected = match &l.sha_url {
        Some(u) => {
            let text = s.http.get(u).header("User-Agent", format!("cuedd/{CURRENT}")).send().await?.error_for_status()?.text().await?;
            text.split_whitespace().next().map(|h| h.to_ascii_lowercase())
        }
        None => None,
    };
    let part = dir.join(format!("{}.part", l.archive_name));
    let resp = s.http.get(&l.archive_url).header("User-Agent", format!("cuedd/{CURRENT}")).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(l.size);
    let mut file = tokio::fs::File::create(&part).await?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        done += chunk.len() as u64;
        if total > 0 {
            let p = (done as f32 / total as f32).min(0.99);
            set_state(s, |u| u.progress = p);
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);
    let actual = hex::encode(hasher.finalize());
    if let Some(exp) = expected {
        if exp != actual {
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("checksum mismatch: the download did not match the published SHA-256");
        }
    } else {
        warn!("update: no .sha256 published for {}; keeping the archive's own hash only", l.archive_name);
    }
    std::fs::rename(&part, dir.join(&l.archive_name))?;
    let meta = Staged { version: l.version.clone(), target: TARGET.into(), archive: l.archive_name.clone(), sha256: actual, staged_at: crate::db::now_ms() };
    std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
    info!("update: {} staged; it installs on the next launch", meta.version);
    set_state(s, |u| {
        u.state = "staged".into();
        u.progress = 1.0;
        u.staged = Some(meta);
    });
    Ok(())
}

/// Background checker: first after a short delay, then every six hours, only while `auto_update` is on.
pub fn spawn(state: Arc<AppState>, shutdown: CancellationToken) {
    tokio::spawn(async move {
        let mut first = true;
        loop {
            let wait = if first { FIRST_CHECK_AFTER } else { CHECK_EVERY };
            first = false;
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(wait) => {}
            }
            if !state.settings().auto_update {
                continue;
            }
            if let Err(e) = check(&state, true).await {
                warn!("update check: {e}");
            }
        }
    });
}

// ---- applying ----------------------------------------------------------------------------------

fn exe_name(base: &str) -> String {
    if cfg!(windows) { format!("{base}.exe") } else { base.to_string() }
}

/// Finds `name` inside an extracted archive (one directory deep, as the release packs it).
fn find_binary(root: &Path, name: &str) -> Option<PathBuf> {
    let direct = root.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let p = entry.path().join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Extracts the archive with the system `tar` (GNU tar / bsdtar; Windows 10+ ships one that opens zips too).
fn extract(archive: &Path, into: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_dir_all(into);
    std::fs::create_dir_all(into)?;
    let out = std::process::Command::new("tar").arg("-xf").arg(archive).arg("-C").arg(into).output().map_err(|e| anyhow::anyhow!("running tar: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("tar failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Puts the new binaries next to the running one: `<exe>` becomes `<exe>.old`, the new file takes its place.
/// Pure file work, so it can be tested; the caller re-executes afterwards.
pub fn swap_binaries(extracted: &Path, exe_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut replaced = Vec::new();
    for base in ["cuedd", "cued-funnel"] {
        let name = exe_name(base);
        let Some(src) = find_binary(extracted, &name) else {
            if base == "cuedd" {
                anyhow::bail!("the archive has no {name}");
            }
            continue;
        };
        let dst = exe_dir.join(&name);
        if base != "cuedd" && !dst.exists() {
            continue; // only replace what is installed
        }
        let fresh = exe_dir.join(format!("{name}.new"));
        let old = exe_dir.join(format!("{name}.old"));
        std::fs::copy(&src, &fresh)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
        }
        let _ = std::fs::remove_file(&old);
        if dst.exists() {
            std::fs::rename(&dst, &old)?;
        }
        if let Err(e) = std::fs::rename(&fresh, &dst) {
            // Put the old one back so the install still works.
            let _ = std::fs::rename(&old, &dst);
            return Err(e.into());
        }
        replaced.push(dst);
    }
    Ok(replaced)
}

/// Applies the staged update now: extract, swap, record, re-execute. Returns only on failure
/// (or when there is nothing to apply, as `Ok(false)`).
pub fn apply_staged(data_dir: &Path) -> anyhow::Result<bool> {
    let Some(staged) = read_staged(data_dir) else { return Ok(false) };
    if cmp_versions(&staged.version, CURRENT) != std::cmp::Ordering::Greater {
        info!("update: staged {} is not newer than {CURRENT}; dropping it", staged.version);
        discard_staged(data_dir);
        return Ok(false);
    }
    if staged.target != TARGET {
        discard_staged(data_dir);
        anyhow::bail!("staged archive is for {} but this build is {TARGET}", staged.target);
    }
    let archive = pending_dir(data_dir).join(&staged.archive);
    let bytes = std::fs::read(&archive)?;
    if hex::encode(Sha256::digest(&bytes)) != staged.sha256 {
        discard_staged(data_dir);
        anyhow::bail!("staged archive changed on disk since it was verified; discarded");
    }
    drop(bytes);
    let extracted = updates_dir(data_dir).join("extract");
    extract(&archive, &extracted)?;
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().ok_or_else(|| anyhow::anyhow!("no directory for {}", exe.display()))?.to_path_buf();
    swap_binaries(&extracted, &exe_dir)?;
    let applied = Applied { from: CURRENT.into(), to: staged.version.clone(), applied_at: crate::db::now_ms() };
    std::fs::write(applied_path(data_dir), serde_json::to_vec_pretty(&applied)?)?;
    let _ = std::fs::remove_file(apply_error_path(data_dir));
    discard_staged(data_dir);
    let _ = std::fs::remove_dir_all(&extracted);
    info!("update: {} installed, restarting", staged.version);
    reexec(&exe)
}

/// Replaces this process with the (new) binary at `exe`, same arguments.
fn reexec(exe: &Path) -> anyhow::Result<bool> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(exe).args(&args).exec();
        anyhow::bail!("could not start the new binary: {err}");
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new(exe).args(&args).spawn().map_err(|e| anyhow::anyhow!("could not start the new binary: {e}"))?;
        std::process::exit(0);
    }
}

/// At startup: apply a staged update if there is one, remembering failures for the UI; tidy `.old` files.
pub fn at_startup(data_dir: &Path) {
    match apply_staged(data_dir) {
        Ok(true) => {}
        Ok(false) => {}
        Err(e) => {
            warn!("update: could not install the staged version: {e}");
            let _ = std::fs::create_dir_all(updates_dir(data_dir));
            let _ = std::fs::write(apply_error_path(data_dir), e.to_string());
        }
    }
    // We are running: the previous binaries can go (Windows may still hold them for a moment; next time then).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for base in ["cuedd", "cued-funnel"] {
                let _ = std::fs::remove_file(dir.join(format!("{}.old", exe_name(base))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_versions("0.3.0", "0.2.9"), Greater);
        assert_eq!(cmp_versions("v0.3.1", "0.3.1"), Equal);
        assert_eq!(cmp_versions("0.3.1-rc1", "0.3.1"), Equal);
        assert_eq!(cmp_versions("0.10.0", "0.9.9"), Greater);
        assert_eq!(cmp_versions("1.0", "1.0.0"), Equal);
        assert_eq!(cmp_versions("0.2.5", "0.3.0"), Less);
    }

    #[test]
    fn changelog_sections_and_notes_between() {
        let log = "# Changelog\n\n## 0.3.1\n\n- one\n\n## 0.3.0\n\n- zero\n- more\n\n## 0.2.5\n\n- old\n";
        let s = sections(log);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].0, "0.3.1");
        assert_eq!(s[1].1, "- zero\n- more");
        let n = notes_between(log, Some("0.2.5"), "0.3.1");
        assert!(n.contains("## 0.3.1") && n.contains("## 0.3.0") && !n.contains("## 0.2.5"));
        let only = notes_between(log, Some("0.3.0"), "0.3.1");
        assert!(only.contains("- one") && !only.contains("- zero"));
        assert!(notes_between(log, None, "0.2.5").contains("- old"));
        // The real changelog has a section for the running version.
        assert!(sections(CHANGELOG).iter().any(|(v, _)| v == CURRENT), "CHANGELOG.md needs a ## {CURRENT} section");
    }

    #[test]
    fn picks_this_targets_archive() {
        let rel = serde_json::json!({
            "tag_name": "v0.3.1", "draft": false, "published_at": "2026-10-15T00:00:00Z", "body": "notes",
            "assets": [
                {"name": "cued-desktop-v0.3.1-x86_64-unknown-linux-gnu.tar.gz", "browser_download_url": "https://x/a.tar.gz", "size": 10},
                {"name": "cued-desktop-v0.3.1-x86_64-unknown-linux-gnu.tar.gz.sha256", "browser_download_url": "https://x/a.sha", "size": 1},
                {"name": "cued-desktop-v0.3.1-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://x/w.zip", "size": 10}
            ]
        });
        let l = pick_asset(&rel, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(l.version, "0.3.1");
        assert_eq!(l.archive_url, "https://x/a.tar.gz");
        assert_eq!(l.sha_url.as_deref(), Some("https://x/a.sha"));
        let w = pick_asset(&rel, "x86_64-pc-windows-msvc").unwrap();
        assert_eq!(w.archive_name, "cued-desktop-v0.3.1-x86_64-pc-windows-msvc.zip");
        assert!(w.sha_url.is_none());
        assert!(pick_asset(&rel, "aarch64-apple-darwin").is_none());
        let mut draft = rel.clone();
        draft["draft"] = serde_json::json!(true);
        assert!(pick_asset(&draft, "x86_64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn swap_replaces_binaries_and_keeps_old() {
        let tmp = tempfile::tempdir().unwrap();
        let extracted = tmp.path().join("extract");
        let inner = extracted.join("cued-desktop-v9.9.9-test");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join(exe_name("cuedd")), b"new daemon").unwrap();
        std::fs::write(inner.join(exe_name("cued-funnel")), b"new funnel").unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(exe_name("cuedd")), b"old daemon").unwrap();
        // No funnel installed here: it must not be added.
        let replaced = swap_binaries(&extracted, &bin).unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(std::fs::read(bin.join(exe_name("cuedd"))).unwrap(), b"new daemon");
        assert_eq!(std::fs::read(bin.join(format!("{}.old", exe_name("cuedd")))).unwrap(), b"old daemon");
        assert!(!bin.join(exe_name("cued-funnel")).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(std::fs::metadata(bin.join("cuedd")).unwrap().permissions().mode() & 0o111 != 0);
        }
    }

    #[test]
    fn staged_meta_round_trip_and_stale_applied() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_staged(tmp.path()).is_none());
        std::fs::create_dir_all(pending_dir(tmp.path())).unwrap();
        std::fs::write(pending_dir(tmp.path()).join("x.tar.gz"), b"...").unwrap();
        let meta = Staged { version: "9.9.9".into(), target: TARGET.into(), archive: "x.tar.gz".into(), sha256: "00".into(), staged_at: 1 };
        std::fs::write(pending_dir(tmp.path()).join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        assert_eq!(read_staged(tmp.path()).unwrap().version, "9.9.9");
        // A hash mismatch is discarded, not installed.
        assert!(apply_staged(tmp.path()).is_err());
        assert!(read_staged(tmp.path()).is_none());
        // applied.json for another version than the one running is dropped.
        std::fs::write(applied_path(tmp.path()), serde_json::to_vec(&Applied { from: "0.0.1".into(), to: "0.0.2".into(), applied_at: 1 }).unwrap()).unwrap();
        assert!(read_applied(tmp.path()).is_none());
        assert!(!applied_path(tmp.path()).exists());
        std::fs::write(applied_path(tmp.path()), serde_json::to_vec(&Applied { from: "0.0.1".into(), to: CURRENT.into(), applied_at: 1 }).unwrap()).unwrap();
        assert_eq!(read_applied(tmp.path()).unwrap().from, "0.0.1");
    }
}
