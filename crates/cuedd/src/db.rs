//! SQLite index: devices, tracks, what each phone has, blobs, follows.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub pubkey: String,
    pub paired_at: i64,
    pub last_seen: i64,
    pub last_manifest: i64,
    pub last_upload: i64,
}

/// One manifest entry from a phone, as in `docs/protocol.md` §3.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ManifestEntry {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
    pub size: i64,
    pub sha256: String,
    pub mime: String,
    pub genres: Vec<String>,
    pub bpm: Option<f64>,
    pub link: Option<String>,
    pub modified: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub sha256: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
    pub size: i64,
    pub mime: String,
    pub genres: Vec<String>,
    pub bpm: Option<f64>,
    pub link: Option<String>,
    pub path: String,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
    /// `phone:<device id>`, `receive`, `companion`, `import`.
    pub source: String,
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub device: Device,
    #[serde(rename = "onPhone")]
    pub on_phone: i64,
    #[serde(rename = "backedUp")]
    pub backed_up: i64,
    pub bytes: i64,
    pub missing: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Follow {
    pub pubkey: String,
    pub name: String,
    pub relays: Vec<String>,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS devices(
                id TEXT PRIMARY KEY, name TEXT NOT NULL, pubkey TEXT NOT NULL DEFAULT '',
                token_hash TEXT NOT NULL UNIQUE, paired_at INTEGER NOT NULL, last_seen INTEGER NOT NULL DEFAULT 0,
                last_manifest INTEGER NOT NULL DEFAULT 0, last_upload INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS tracks(
                sha256 TEXT PRIMARY KEY, title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT NOT NULL DEFAULT '',
                duration_ms INTEGER NOT NULL DEFAULT 0, size INTEGER NOT NULL, mime TEXT NOT NULL, genres TEXT NOT NULL DEFAULT '[]',
                bpm REAL, link TEXT, path TEXT NOT NULL, added_at INTEGER NOT NULL, source TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS tracks_artist ON tracks(artist, album, title);
            CREATE TABLE IF NOT EXISTS device_tracks(
                device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE, sha256 TEXT NOT NULL, key TEXT NOT NULL DEFAULT '',
                present INTEGER NOT NULL DEFAULT 1, entry TEXT NOT NULL, modified INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(device_id, sha256));
            CREATE TABLE IF NOT EXISTS blobs(
                device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE, name TEXT NOT NULL, size INTEGER NOT NULL,
                updated_at INTEGER NOT NULL, PRIMARY KEY(device_id, name));
            CREATE TABLE IF NOT EXISTS follows(pubkey TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', relays TEXT NOT NULL DEFAULT '[]', added_at INTEGER NOT NULL);
            "#,
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> anyhow::Result<T> {
        let c = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        Ok(f(&c)?)
    }

    // ---- devices ----

    pub fn add_device(&self, id: &str, name: &str, pubkey: &str, token_hash: &str) -> anyhow::Result<Device> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO devices(id,name,pubkey,token_hash,paired_at,last_seen) VALUES(?1,?2,?3,?4,?5,?5)",
                params![id, name, pubkey, token_hash, now],
            )
        })?;
        Ok(Device { id: id.into(), name: name.into(), pubkey: pubkey.into(), paired_at: now, last_seen: now, last_manifest: 0, last_upload: 0 })
    }

    pub fn device_by_token_hash(&self, token_hash: &str) -> anyhow::Result<Option<Device>> {
        self.with(|c| {
            c.query_row("SELECT id,name,pubkey,paired_at,last_seen,last_manifest,last_upload FROM devices WHERE token_hash=?1", params![token_hash], row_device).optional()
        })
    }

    pub fn device(&self, id: &str) -> anyhow::Result<Option<Device>> {
        self.with(|c| c.query_row("SELECT id,name,pubkey,paired_at,last_seen,last_manifest,last_upload FROM devices WHERE id=?1", params![id], row_device).optional())
    }

    pub fn devices(&self) -> anyhow::Result<Vec<Device>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id,name,pubkey,paired_at,last_seen,last_manifest,last_upload FROM devices ORDER BY paired_at")?;
            let rows = st.query_map([], row_device)?;
            rows.collect()
        })
    }

    pub fn remove_device(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM devices WHERE id=?1", params![id]))? > 0)
    }

    pub fn touch_device(&self, id: &str, manifest: bool, upload: bool) -> anyhow::Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "UPDATE devices SET last_seen=?2, last_manifest=CASE WHEN ?3 THEN ?2 ELSE last_manifest END, last_upload=CASE WHEN ?4 THEN ?2 ELSE last_upload END WHERE id=?1",
                params![id, now, manifest, upload],
            )
        })?;
        Ok(())
    }

    // ---- manifests ----

    /// Replaces the device's manifest. Returns the hashes we do not have a file for.
    pub fn set_manifest(&self, device_id: &str, entries: &[ManifestEntry]) -> anyhow::Result<Vec<String>> {
        self.with(|c| {
            c.execute("UPDATE device_tracks SET present=0 WHERE device_id=?1", params![device_id])?;
            let mut up = c.prepare(
                "INSERT INTO device_tracks(device_id,sha256,key,present,entry,modified) VALUES(?1,?2,?3,1,?4,?5)
                 ON CONFLICT(device_id,sha256) DO UPDATE SET key=excluded.key, present=1, entry=excluded.entry, modified=excluded.modified",
            )?;
            let mut have = c.prepare("SELECT 1 FROM tracks WHERE sha256=?1")?;
            let mut missing = Vec::new();
            for e in entries {
                up.execute(params![device_id, e.sha256, e.key, serde_json::to_string(e).unwrap_or_default(), e.modified])?;
                if !have.exists(params![e.sha256])? {
                    missing.push(e.sha256.clone());
                }
            }
            Ok(missing)
        })
    }

    pub fn manifest(&self, device_id: &str) -> anyhow::Result<Vec<ManifestEntry>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT entry FROM device_tracks WHERE device_id=?1 AND present=1 ORDER BY sha256")?;
            let rows = st.query_map(params![device_id], |r| r.get::<_, String>(0))?;
            Ok(rows.filter_map(|r| r.ok()).filter_map(|s| serde_json::from_str(&s).ok()).collect())
        })
    }

    pub fn manifest_entry(&self, device_id: &str, sha256: &str) -> anyhow::Result<Option<ManifestEntry>> {
        self.with(|c| {
            c.query_row("SELECT entry FROM device_tracks WHERE device_id=?1 AND sha256=?2", params![device_id, sha256], |r| r.get::<_, String>(0))
                .optional()
                .map(|o| o.and_then(|s| serde_json::from_str(&s).ok()))
        })
    }

    pub fn device_status(&self, d: &Device) -> anyhow::Result<DeviceStatus> {
        self.with(|c| {
            let on_phone: i64 = c.query_row("SELECT COUNT(*) FROM device_tracks WHERE device_id=?1 AND present=1", params![d.id], |r| r.get(0))?;
            let (backed_up, bytes): (i64, i64) = c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(t.size),0) FROM device_tracks dt JOIN tracks t ON t.sha256=dt.sha256 WHERE dt.device_id=?1",
                params![d.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let missing: i64 = c.query_row(
                "SELECT COUNT(*) FROM device_tracks dt WHERE dt.device_id=?1 AND dt.present=1 AND NOT EXISTS(SELECT 1 FROM tracks t WHERE t.sha256=dt.sha256)",
                params![d.id],
                |r| r.get(0),
            )?;
            Ok(DeviceStatus { device: d.clone(), on_phone, backed_up, bytes, missing })
        })
    }

    // ---- tracks ----

    pub fn has_track(&self, sha256: &str) -> anyhow::Result<bool> {
        self.with(|c| c.prepare("SELECT 1 FROM tracks WHERE sha256=?1")?.exists(params![sha256]))
    }

    pub fn add_track(&self, t: &Track) -> anyhow::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO tracks(sha256,title,artist,album,duration_ms,size,mime,genres,bpm,link,path,added_at,source) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(sha256) DO UPDATE SET title=excluded.title, artist=excluded.artist, album=excluded.album, duration_ms=excluded.duration_ms, genres=excluded.genres, bpm=COALESCE(excluded.bpm,tracks.bpm), link=COALESCE(excluded.link,tracks.link), path=excluded.path",
                params![t.sha256, t.title, t.artist, t.album, t.duration_ms, t.size, t.mime, serde_json::to_string(&t.genres).unwrap_or_default(), t.bpm, t.link, t.path, t.added_at, t.source],
            )?;
            Ok(())
        })
    }

    pub fn set_bpm(&self, sha256: &str, bpm: f64) -> anyhow::Result<()> {
        self.with(|c| c.execute("UPDATE tracks SET bpm=?2 WHERE sha256=?1", params![sha256, bpm]))?;
        Ok(())
    }

    pub fn track(&self, sha256: &str) -> anyhow::Result<Option<Track>> {
        self.with(|c| {
            let t = c.query_row("SELECT sha256,title,artist,album,duration_ms,size,mime,genres,bpm,link,path,added_at,source FROM tracks WHERE sha256=?1", params![sha256], row_track).optional()?;
            match t {
                Some(mut t) => {
                    t.devices = devices_of(c, &t.sha256)?;
                    Ok(Some(t))
                }
                None => Ok(None),
            }
        })
    }

    pub fn tracks(&self, q: &str, device: Option<&str>, offset: i64, limit: i64) -> anyhow::Result<(Vec<Track>, i64)> {
        let like = format!("%{}%", q.trim());
        self.with(|c| {
            let (where_sql, dev) = match device {
                Some(d) => (" AND EXISTS(SELECT 1 FROM device_tracks dt WHERE dt.sha256=t.sha256 AND dt.device_id=?2)", d.to_string()),
                None => (" AND ?2=?2", String::new()),
            };
            let total: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM tracks t WHERE (t.title LIKE ?1 OR t.artist LIKE ?1 OR t.album LIKE ?1){where_sql}"),
                params![like, dev],
                |r| r.get(0),
            )?;
            let mut st = c.prepare(&format!(
                "SELECT sha256,title,artist,album,duration_ms,size,mime,genres,bpm,link,path,added_at,source FROM tracks t WHERE (t.title LIKE ?1 OR t.artist LIKE ?1 OR t.album LIKE ?1){where_sql} ORDER BY artist, album, title LIMIT ?3 OFFSET ?4"
            ))?;
            let rows = st.query_map(params![like, dev, limit, offset], row_track)?;
            let mut out = Vec::new();
            for r in rows {
                let mut t = r?;
                t.devices = devices_of(c, &t.sha256)?;
                out.push(t);
            }
            Ok((out, total))
        })
    }

    pub fn track_count(&self) -> anyhow::Result<(i64, i64)> {
        self.with(|c| c.query_row("SELECT COUNT(*), COALESCE(SUM(size),0) FROM tracks", [], |r| Ok((r.get(0)?, r.get(1)?))))
    }

    /// Marks that `device` has `sha256` (used after an upload, so a phone that never sent a manifest still shows up).
    pub fn link_device_track(&self, device_id: &str, entry: &ManifestEntry) -> anyhow::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO device_tracks(device_id,sha256,key,present,entry,modified) VALUES(?1,?2,?3,1,?4,?5)
                 ON CONFLICT(device_id,sha256) DO UPDATE SET present=1, entry=excluded.entry",
                params![device_id, entry.sha256, entry.key, serde_json::to_string(entry).unwrap_or_default(), entry.modified],
            )
        })?;
        Ok(())
    }

    // ---- blobs ----

    pub fn put_blob(&self, device_id: &str, name: &str, size: i64) -> anyhow::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO blobs(device_id,name,size,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(device_id,name) DO UPDATE SET size=excluded.size, updated_at=excluded.updated_at",
                params![device_id, name, size, now_ms()],
            )
        })?;
        Ok(())
    }

    pub fn blobs(&self, device_id: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT name,size,updated_at FROM blobs WHERE device_id=?1 ORDER BY name")?;
            let rows = st.query_map(params![device_id], |r| Ok(serde_json::json!({"name": r.get::<_, String>(0)?, "size": r.get::<_, i64>(1)?, "updatedAt": r.get::<_, i64>(2)?})))?;
            rows.collect()
        })
    }

    // ---- follows ----

    pub fn follows(&self) -> anyhow::Result<Vec<Follow>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT pubkey,name,relays,added_at FROM follows ORDER BY added_at")?;
            let rows = st.query_map([], |r| {
                Ok(Follow {
                    pubkey: r.get(0)?,
                    name: r.get(1)?,
                    relays: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    added_at: r.get(3)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn add_follow(&self, f: &Follow) -> anyhow::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO follows(pubkey,name,relays,added_at) VALUES(?1,?2,?3,?4) ON CONFLICT(pubkey) DO UPDATE SET name=CASE WHEN excluded.name='' THEN follows.name ELSE excluded.name END, relays=excluded.relays",
                params![f.pubkey, f.name, serde_json::to_string(&f.relays).unwrap_or_default(), if f.added_at == 0 { now_ms() } else { f.added_at }],
            )
        })?;
        Ok(())
    }

    pub fn remove_follow(&self, pubkey: &str) -> anyhow::Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM follows WHERE pubkey=?1", params![pubkey]))? > 0)
    }
}

fn row_device(r: &rusqlite::Row) -> rusqlite::Result<Device> {
    Ok(Device { id: r.get(0)?, name: r.get(1)?, pubkey: r.get(2)?, paired_at: r.get(3)?, last_seen: r.get(4)?, last_manifest: r.get(5)?, last_upload: r.get(6)? })
}

fn row_track(r: &rusqlite::Row) -> rusqlite::Result<Track> {
    Ok(Track {
        sha256: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        duration_ms: r.get(4)?,
        size: r.get(5)?,
        mime: r.get(6)?,
        genres: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        bpm: r.get(8)?,
        link: r.get(9)?,
        path: r.get(10)?,
        added_at: r.get(11)?,
        source: r.get(12)?,
        devices: vec![],
    })
}

fn devices_of(c: &Connection, sha256: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare("SELECT device_id FROM device_tracks WHERE sha256=?1 AND present=1")?;
    let rows = st.query_map(params![sha256], |r| r.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str, title: &str) -> ManifestEntry {
        ManifestEntry { key: title.into(), title: title.into(), artist: "A".into(), sha256: sha.into(), size: 10, mime: "audio/mpeg".into(), ..Default::default() }
    }

    #[test]
    fn manifest_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let d = db.add_device("0011223344556677", "Pixel", "", "h1").unwrap();
        assert_eq!(db.device_by_token_hash("h1").unwrap().unwrap().id, d.id);
        let missing = db.set_manifest(&d.id, &[entry("aa", "One"), entry("bb", "Two")]).unwrap();
        assert_eq!(missing, vec!["aa", "bb"]);
        db.add_track(&Track { sha256: "aa".into(), title: "One".into(), artist: "A".into(), album: "".into(), duration_ms: 0, size: 10, mime: "audio/mpeg".into(), genres: vec![], bpm: None, link: None, path: "x".into(), added_at: 1, source: "phone:0011223344556677".into(), devices: vec![] }).unwrap();
        let s = db.device_status(&d).unwrap();
        assert_eq!((s.on_phone, s.backed_up, s.missing, s.bytes), (2, 1, 1, 10));
        // Track leaves the phone: still backed up, no longer "on phone".
        db.set_manifest(&d.id, &[entry("bb", "Two")]).unwrap();
        let s = db.device_status(&d).unwrap();
        assert_eq!((s.on_phone, s.backed_up), (1, 1));
        let (tracks, total) = db.tracks("one", None, 0, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(tracks[0].sha256, "aa");
        assert!(db.remove_device(&d.id).unwrap());
        assert!(db.devices().unwrap().is_empty());
    }
}
