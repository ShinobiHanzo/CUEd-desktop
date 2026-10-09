//! SQLite storage. Every function takes the open connection; the `Relay`
//! serialises access behind one mutex because each query is tiny.

use std::path::Path;

use anyhow::Context as _;
use cued_proto::Event;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension};

use crate::filter::Filter;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id          TEXT PRIMARY KEY,
    pubkey      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    kind        INTEGER NOT NULL,
    tags_json   TEXT NOT NULL,
    content     TEXT NOT NULL,
    sig         TEXT NOT NULL,
    d_tag       TEXT,
    received_at INTEGER NOT NULL,
    size        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS events_pubkey_kind ON events(pubkey, kind);
CREATE INDEX IF NOT EXISTS events_created_at ON events(created_at);
CREATE INDEX IF NOT EXISTS events_kind ON events(kind);
CREATE TABLE IF NOT EXISTS tags (
    event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    value    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tags_name_value ON tags(name, value);
CREATE INDEX IF NOT EXISTS tags_event ON tags(event_id);
";

pub(crate) fn open(path: Option<&Path>) -> anyhow::Result<Connection> {
    let conn = match path {
        Some(p) => {
            if let Some(dir) = p.parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
                }
            }
            let c = Connection::open(p).with_context(|| format!("opening relay database {}", p.display()))?;
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "synchronous", "NORMAL")?;
            c
        }
        None => Connection::open_in_memory().context("opening in-memory relay database")?,
    };
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA).context("creating relay schema")?;
    Ok(conn)
}

const COLUMNS: &str = "id, pubkey, created_at, kind, tags_json, content, sig";

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let tags_json: String = row.get(4)?;
    let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();
    Ok(Event {
        id: row.get(0)?,
        pubkey: row.get(1)?,
        created_at: row.get(2)?,
        kind: row.get::<_, i64>(3)? as u32,
        tags,
        content: row.get(5)?,
        sig: row.get(6)?,
    })
}

pub(crate) fn has_event(c: &Connection, id: &str) -> rusqlite::Result<bool> {
    c.query_row("SELECT 1 FROM events WHERE id = ?1", params![id], |_| Ok(())).optional().map(|o| o.is_some())
}

/// Newest event by this author and kind, optionally pinned to a `d` tag.
pub(crate) fn latest(c: &Connection, kind: u32, pubkey: &str, d_tag: Option<&str>) -> rusqlite::Result<Option<Event>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM events WHERE pubkey = ?1 AND kind = ?2 {} ORDER BY created_at DESC, id ASC LIMIT 1",
        if d_tag.is_some() { "AND d_tag = ?3" } else { "" }
    );
    let mut stmt = c.prepare_cached(&sql)?;
    match d_tag {
        Some(d) => stmt.query_row(params![pubkey, kind, d], row_to_event).optional(),
        None => stmt.query_row(params![pubkey, kind], row_to_event).optional(),
    }
}

/// Removes every event this one replaces (same author and kind, same `d` when given).
pub(crate) fn delete_replaced(c: &Connection, kind: u32, pubkey: &str, d_tag: Option<&str>) -> rusqlite::Result<usize> {
    match d_tag {
        Some(d) => c.execute("DELETE FROM events WHERE pubkey = ?1 AND kind = ?2 AND d_tag = ?3", params![pubkey, kind, d]),
        None => c.execute("DELETE FROM events WHERE pubkey = ?1 AND kind = ?2", params![pubkey, kind]),
    }
}

pub(crate) fn insert(c: &Connection, ev: &Event, size: usize, received_at: i64) -> rusqlite::Result<()> {
    let tags_json = serde_json::to_string(&ev.tags).unwrap_or_else(|_| "[]".into());
    let d_tag = if ev.is_param_replaceable() { Some(ev.tag("d").unwrap_or("")) } else { ev.tag("d") };
    c.execute(
        "INSERT INTO events (id, pubkey, created_at, kind, tags_json, content, sig, d_tag, received_at, size)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![ev.id, ev.pubkey, ev.created_at, ev.kind, tags_json, ev.content, ev.sig, d_tag, received_at, size as i64],
    )?;
    let mut stmt = c.prepare_cached("INSERT INTO tags (event_id, name, value) VALUES (?1, ?2, ?3)")?;
    for t in ev.tags.iter().filter(|t| t.len() >= 2 && !t[0].is_empty()) {
        stmt.execute(params![ev.id, t[0], t[1]])?;
    }
    Ok(())
}

fn placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s
}

/// Stored events matching one filter, newest first, at most `filter.limit`.
pub(crate) fn query(c: &Connection, filter: &Filter) -> rusqlite::Result<Vec<Event>> {
    if filter.is_empty_match() || filter.limit == 0 {
        return Ok(Vec::new());
    }
    let mut sql = format!("SELECT {COLUMNS} FROM events e WHERE 1=1");
    let mut args: Vec<SqlValue> = Vec::new();
    let mut set = |sql: &mut String, col: &str, values: &std::collections::HashSet<String>| {
        sql.push_str(&format!(" AND e.{col} IN ({})", placeholders(values.len())));
        args.extend(values.iter().map(|v| SqlValue::Text(v.clone())));
    };
    if let Some(ids) = &filter.ids {
        set(&mut sql, "id", ids);
    }
    if let Some(authors) = &filter.authors {
        set(&mut sql, "pubkey", authors);
    }
    if let Some(kinds) = &filter.kinds {
        sql.push_str(&format!(" AND e.kind IN ({})", placeholders(kinds.len())));
        args.extend(kinds.iter().map(|k| SqlValue::Integer(i64::from(*k))));
    }
    for (name, values) in &filter.tags {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM tags t WHERE t.event_id = e.id AND t.name = ? AND t.value IN ({}))",
            placeholders(values.len())
        ));
        args.push(SqlValue::Text(name.clone()));
        args.extend(values.iter().map(|v| SqlValue::Text(v.clone())));
    }
    if let Some(since) = filter.since {
        sql.push_str(" AND e.created_at >= ?");
        args.push(SqlValue::Integer(since));
    }
    if let Some(until) = filter.until {
        sql.push_str(" AND e.created_at <= ?");
        args.push(SqlValue::Integer(until));
    }
    sql.push_str(" ORDER BY e.created_at DESC, e.id ASC LIMIT ?");
    args.push(SqlValue::Integer(filter.limit as i64));
    let mut stmt = c.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), row_to_event)?;
    rows.collect()
}

pub(crate) fn count(c: &Connection) -> rusqlite::Result<u64> {
    c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)).map(|n| n.max(0) as u64)
}

pub(crate) fn bytes(c: &Connection) -> rusqlite::Result<u64> {
    c.query_row("SELECT COALESCE(SUM(size), 0) FROM events", [], |r| r.get::<_, i64>(0)).map(|n| n.max(0) as u64)
}
