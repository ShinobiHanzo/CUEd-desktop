//! Content-addressed file store with resumable uploads.
//!
//! Finished files live at `library/<artist>/<album>/<artist> - <title>.<ext>`;
//! an upload in flight is `tmp/<sha256>.part`. A file is only indexed once its
//! SHA-256 matches what the sender claimed.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Store {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("resume offset {offset} does not match stored {have}")]
    BadOffset { offset: u64, have: u64 },
    #[error("hash mismatch: expected {expected}, got {got}")]
    HashMismatch { expected: String, got: String },
}

impl Store {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root.join("library"))?;
        std::fs::create_dir_all(root.join("tmp"))?;
        std::fs::create_dir_all(root.join("backup"))?;
        std::fs::create_dir_all(root.join("downloads"))?;
        Ok(Self { root: root.to_path_buf() })
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }
    pub fn backup_dir(&self, device: &str) -> PathBuf {
        self.root.join("backup").join(safe_segment(device))
    }
    pub fn blob_path(&self, device: &str, name: &str) -> PathBuf {
        self.backup_dir(device).join("blobs").join(safe_segment(name))
    }

    fn part_path(&self, sha256: &str) -> PathBuf {
        self.root.join("tmp").join(format!("{}.part", safe_segment(sha256)))
    }

    /// Bytes already received for an upload in flight.
    pub async fn have(&self, sha256: &str) -> u64 {
        tokio::fs::metadata(self.part_path(sha256)).await.map(|m| m.len()).unwrap_or(0)
    }

    /// Opens (or resumes) the partial file at `offset`.
    pub async fn begin(&self, sha256: &str, offset: u64) -> Result<tokio::fs::File, StoreError> {
        let p = self.part_path(sha256);
        let have = self.have(sha256).await;
        if offset != have {
            return Err(StoreError::BadOffset { offset, have });
        }
        if offset == 0 {
            Ok(tokio::fs::File::create(p).await?)
        } else {
            Ok(tokio::fs::OpenOptions::new().append(true).open(p).await?)
        }
    }

    #[allow(dead_code)]
    pub async fn discard(&self, sha256: &str) {
        let _ = tokio::fs::remove_file(self.part_path(sha256)).await;
    }

    /// Verifies the partial file against `sha256` and moves it into the library. Returns the final path.
    pub async fn finish(&self, sha256: &str, artist: &str, album: &str, title: &str, ext: &str) -> Result<PathBuf, StoreError> {
        let part = self.part_path(sha256);
        let got = hash_file(&part).await?;
        if got != sha256.to_ascii_lowercase() {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(StoreError::HashMismatch { expected: sha256.into(), got });
        }
        let dest = self.library_path(artist, album, title, ext, sha256).await?;
        tokio::fs::rename(&part, &dest).await?;
        Ok(dest)
    }

    /// Moves an already complete file (companion download, receive) into the library, hashing it on the way.
    pub async fn adopt(&self, file: &Path, artist: &str, album: &str, title: &str) -> Result<(String, PathBuf, u64), StoreError> {
        let sha = hash_file(file).await?;
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let dest = self.library_path(artist, album, title, ext, &sha).await?;
        if dest != file
            && tokio::fs::rename(file, &dest).await.is_err() {
                tokio::fs::copy(file, &dest).await?;
                let _ = tokio::fs::remove_file(file).await;
            }
        let size = tokio::fs::metadata(&dest).await?.len();
        Ok((sha, dest, size))
    }

    async fn library_path(&self, artist: &str, album: &str, title: &str, ext: &str, sha256: &str) -> std::io::Result<PathBuf> {
        let artist = non_empty(safe_segment(artist), "Unknown artist");
        let album = non_empty(safe_segment(album), "Unknown album");
        let title = non_empty(safe_segment(title), &sha256[..12.min(sha256.len())]);
        let dir = self.root.join("library").join(&artist).join(&album);
        tokio::fs::create_dir_all(&dir).await?;
        let mut dest = dir.join(format!("{artist} - {title}.{ext}"));
        if tokio::fs::try_exists(&dest).await? {
            dest = dir.join(format!("{artist} - {title} [{}].{ext}", &sha256[..8.min(sha256.len())]));
        }
        Ok(dest)
    }
}

fn non_empty(s: String, fallback: &str) -> String {
    if s.trim().is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// A file-system-safe single path segment: no separators, no control characters, no leading dots, ≤ 80 chars.
pub fn safe_segment(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .trim_start_matches(['.', ' '])
        .chars()
        .take(80)
        .collect();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    out
}

pub async fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub async fn write_all(f: &mut tokio::fs::File, data: &[u8]) -> std::io::Result<()> {
    f.write_all(data).await
}

/// File extension for a MIME type the phone reports.
pub fn ext_for_mime(mime: &str, fallback: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "audio/mp4" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        _ => match fallback {
            "m4a" => "m4a",
            "mp3" => "mp3",
            "flac" => "flac",
            "ogg" => "ogg",
            "opus" => "opus",
            "wav" => "wav",
            _ => "bin",
        },
    }
}

pub fn mime_for_path(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("m4a") | Some("mp4") => "audio/mp4".into(),
        Some("mp3") => "audio/mpeg".into(),
        Some("flac") => "audio/flac".into(),
        Some("ogg") | Some("oga") => "audio/ogg".into(),
        Some("opus") => "audio/opus".into(),
        Some("wav") => "audio/wav".into(),
        _ => mime_guess::from_path(path).first_or_octet_stream().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resumable_upload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let data = b"hello cued desktop".to_vec();
        let sha = hex::encode(Sha256::digest(&data));
        let mut f = store.begin(&sha, 0).await.unwrap();
        write_all(&mut f, &data[..5]).await.unwrap();
        drop(f);
        assert_eq!(store.have(&sha).await, 5);
        assert!(matches!(store.begin(&sha, 3).await, Err(StoreError::BadOffset { .. })));
        let mut f = store.begin(&sha, 5).await.unwrap();
        write_all(&mut f, &data[5..]).await.unwrap();
        drop(f);
        let dest = store.finish(&sha, "Evanescence", "Fallen", "Going Under", "mp3").await.unwrap();
        assert!(dest.ends_with("library/Evanescence/Fallen/Evanescence - Going Under.mp3"), "{dest:?}");
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), data);
        assert_eq!(store.have(&sha).await, 0);
    }

    #[tokio::test]
    async fn hash_mismatch_drops_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let mut f = store.begin("00", 0).await.unwrap();
        write_all(&mut f, b"x").await.unwrap();
        drop(f);
        assert!(matches!(store.finish("00", "a", "b", "c", "mp3").await, Err(StoreError::HashMismatch { .. })));
        assert_eq!(store.have("00").await, 0);
    }

    #[test]
    fn segments_are_safe() {
        assert_eq!(safe_segment("../etc/passwd"), "_etc_passwd");
        assert_eq!(safe_segment("AC/DC: Back In Black?"), "AC_DC_ Back In Black_");
        assert_eq!(safe_segment("  . hidden. "), "hidden");
        assert_eq!(ext_for_mime("audio/mp4", ""), "m4a");
        assert_eq!(ext_for_mime("text/plain", "flac"), "flac");
    }
}
