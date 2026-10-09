//! Cheap metadata for files that arrive without a manifest entry (companion
//! downloads, receives): `ffprobe` when present, the file name otherwise.

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Probed {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: i64,
    pub genres: Vec<String>,
    pub link: Option<String>,
}

pub async fn probe(path: &Path) -> Probed {
    let mut p = from_filename(path);
    if let Some(ff) = ffprobe(path).await {
        let fmt = &ff["format"];
        let tags = &fmt["tags"];
        let get = |k: &str| tags.get(k).or_else(|| tags.get(k.to_uppercase())).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let Some(t) = get("title") {
            p.title = t;
        }
        if let Some(a) = get("artist").or_else(|| get("album_artist")) {
            p.artist = a;
        }
        if let Some(a) = get("album") {
            p.album = a;
        }
        if let Some(g) = get("genre") {
            p.genres = g.split([';', ',', '/']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        if let Some(c) = get("comment").or_else(|| get("description")) {
            if c.starts_with("http") {
                p.link = Some(c.split_whitespace().next().unwrap_or("").to_string());
            }
        }
        if let Some(d) = fmt["duration"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            p.duration_ms = (d * 1000.0) as i64;
        }
    }
    p
}

fn from_filename(path: &Path) -> Probed {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let (artist, title) = match stem.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), stem.clone()),
    };
    Probed { title, artist, ..Default::default() }
}

async fn ffprobe(path: &Path) -> Option<serde_json::Value> {
    let out = tokio::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_format", "-print_format", "json"])
        .arg(path)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}


/// The embedded picture as a JPEG/PNG file under `<data>/cache/covers/<sha>.jpg`, extracted once. None when the file has none.
pub async fn cover_file(data_dir: &Path, sha: &str, audio: &Path) -> Option<std::path::PathBuf> {
    let dir = data_dir.join("cache").join("covers");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let out = dir.join(format!("{sha}.jpg"));
    let none = dir.join(format!("{sha}.none"));
    if tokio::fs::try_exists(&out).await.unwrap_or(false) {
        return Some(out);
    }
    if tokio::fs::try_exists(&none).await.unwrap_or(false) {
        return None;
    }
    let status = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(audio)
        .args(["-an", "-map", "0:v:0", "-c:v", "copy", "-f", "image2", "-frames:v", "1"])
        .arg(&out)
        .output()
        .await;
    match status {
        Ok(o) if o.status.success() && tokio::fs::metadata(&out).await.map(|m| m.len() > 0).unwrap_or(false) => Some(out),
        _ => {
            let _ = tokio::fs::remove_file(&out).await;
            let _ = tokio::fs::write(&none, b"").await;
            None
        }
    }
}

/// Lyrics: a `.lrc` sidecar next to the file, else an embedded `lyrics…` tag. `(text, synced)`.
pub async fn lyrics(audio: &Path) -> Option<(String, bool)> {
    let lrc = audio.with_extension("lrc");
    if let Ok(t) = tokio::fs::read_to_string(&lrc).await {
        if !t.trim().is_empty() {
            return Some((t.clone(), is_synced(&t)));
        }
    }
    let ff = ffprobe(audio).await?;
    let tags = ff["format"]["tags"].as_object()?;
    for (k, v) in tags {
        let kl = k.to_ascii_lowercase();
        if kl.starts_with("lyrics") || kl == "uslt" || kl == "unsyncedlyrics" || kl == "©lyr" {
            if let Some(t) = v.as_str().filter(|t| !t.trim().is_empty()) {
                return Some((t.to_string(), is_synced(t)));
            }
        }
    }
    None
}

fn is_synced(t: &str) -> bool {
    t.lines().take(20).any(|l| l.trim_start().starts_with('[') && l.chars().nth(3) == Some(':'))
}
