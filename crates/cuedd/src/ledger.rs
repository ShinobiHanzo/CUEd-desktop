//! Your listening ledger: everything the play log says about a period,
//! computed here from the raw rows. Nothing leaves the machine; the export is a
//! JSON file you own. Mirrors `dev.cued.core.ledger.Ledger` on the phone so both
//! show the same numbers for the same plays.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, Local, TimeZone, Timelike};
use serde::Serialize;

use crate::db::PlayRow;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub plays: i64,
    #[serde(rename = "listenedMs")]
    pub listened_ms: i64,
    pub sub: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    #[serde(rename = "fromMs")]
    pub from_ms: i64,
    #[serde(rename = "toMs")]
    pub to_ms: i64,
    pub plays: i64,
    pub skips: i64,
    #[serde(rename = "listenedMs")]
    pub listened_ms: i64,
    #[serde(rename = "uniqueTracks")]
    pub unique_tracks: i64,
    #[serde(rename = "uniqueArtists")]
    pub unique_artists: i64,
    pub discoveries: i64,
    #[serde(rename = "activeDays")]
    pub active_days: i64,
    #[serde(rename = "longestStreakDays")]
    pub longest_streak_days: i64,
    #[serde(rename = "currentStreakDays")]
    pub current_streak_days: i64,
    #[serde(rename = "byHour")]
    pub by_hour: Vec<i64>,
    #[serde(rename = "byWeekday")]
    pub by_weekday: Vec<i64>,
    /// `[epochDay, plays]`, oldest first.
    #[serde(rename = "byDay")]
    pub by_day: Vec<[i64; 2]>,
    #[serde(rename = "topArtists")]
    pub top_artists: Vec<Entry>,
    #[serde(rename = "topTracks")]
    pub top_tracks: Vec<Entry>,
    #[serde(rename = "topAlbums")]
    pub top_albums: Vec<Entry>,
    #[serde(rename = "topGenres")]
    pub top_genres: Vec<Entry>,
    #[serde(rename = "peakDay")]
    pub peak_day: Option<i64>,
    #[serde(rename = "peakDayPlays")]
    pub peak_day_plays: i64,
    #[serde(rename = "skipRate")]
    pub skip_rate: f32,
}

#[derive(Default)]
struct Acc {
    name: Option<String>,
    sub: String,
    plays: i64,
    ms: i64,
}

/// Local-time period presets as `[from, to)` millis: `month`, `last-month`, `year`, `7d`, `30d`, `all`.
pub fn period(name: &str, now_ms: i64) -> (i64, i64) {
    let now = Local.timestamp_millis_opt(now_ms).single().unwrap_or_else(Local::now);
    let day = now.date_naive();
    let start_of = |d: chrono::NaiveDate| Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).single().map(|t| t.timestamp_millis()).unwrap_or(0);
    match name {
        "last-month" => {
            let first = day.with_day(1).unwrap();
            let prev = first.pred_opt().unwrap().with_day(1).unwrap();
            (start_of(prev), start_of(first))
        }
        "year" => {
            let first = day.with_ordinal(1).unwrap();
            let next = chrono::NaiveDate::from_ymd_opt(first.year() + 1, 1, 1).unwrap();
            (start_of(first), start_of(next))
        }
        "7d" | "30d" => {
            let days: i64 = if name == "7d" { 7 } else { 30 };
            let end = day.succ_opt().unwrap();
            (start_of(end - chrono::Duration::days(days)), start_of(end))
        }
        "all" => (0, i64::MAX / 4),
        _ => {
            let first = day.with_day(1).unwrap();
            let next = if first.month() == 12 { chrono::NaiveDate::from_ymd_opt(first.year() + 1, 1, 1) } else { first.with_month(first.month() + 1) }.unwrap();
            (start_of(first), start_of(next))
        }
    }
}

fn epoch_day(ms: i64) -> (i64, u32, u32) {
    let t = Local.timestamp_millis_opt(ms).single().unwrap_or_else(Local::now);
    let days = t.date_naive().signed_duration_since(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days();
    (days, t.hour(), t.weekday().num_days_from_monday())
}

/// Longest run of consecutive active days, and the run that ends today or yesterday.
pub fn streaks(days: &[i64], today: i64) -> (i64, i64) {
    if days.is_empty() {
        return (0, 0);
    }
    let mut d: Vec<i64> = days.to_vec();
    d.sort_unstable();
    d.dedup();
    let (mut longest, mut run) = (1i64, 1i64);
    for i in 1..d.len() {
        run = if d[i] == d[i - 1] + 1 { run + 1 } else { 1 };
        longest = longest.max(run);
    }
    let last = *d.last().unwrap();
    let current = if last == today || last == today - 1 { run } else { 0 };
    (longest, current)
}

/// `all` is the whole history (so a first-ever play inside the period counts as a discovery); only rows inside `[from, to)` are summarised.
pub fn summarise(all: &[PlayRow], from_ms: i64, to_ms: i64, now_ms: i64, top: usize) -> Summary {
    let mut first_play: HashMap<&str, i64> = HashMap::new();
    for p in all {
        first_play.entry(&p.sha256).and_modify(|v| *v = (*v).min(p.started_at)).or_insert(p.started_at);
    }
    let mut rows: Vec<&PlayRow> = all.iter().filter(|p| p.started_at >= from_ms && p.started_at < to_ms).collect();
    rows.sort_by_key(|p| p.started_at);

    let mut by_hour = vec![0i64; 24];
    let mut by_weekday = vec![0i64; 7];
    let mut by_day: BTreeMap<i64, i64> = BTreeMap::new();
    let mut artists: HashMap<String, Acc> = HashMap::new();
    let mut tracks: HashMap<String, Acc> = HashMap::new();
    let mut albums: HashMap<String, Acc> = HashMap::new();
    let mut genres: HashMap<String, Acc> = HashMap::new();
    let (mut skips, mut listened, mut discoveries) = (0i64, 0i64, 0i64);
    let mut seen_tracks: HashSet<&str> = HashSet::new();
    let mut seen_artists: HashSet<String> = HashSet::new();

    for p in &rows {
        let (day, hour, wd) = epoch_day(p.started_at);
        by_hour[hour as usize] += 1;
        by_weekday[wd as usize] += 1;
        *by_day.entry(day).or_insert(0) += 1;
        // A skip is a play that ended before half the track (or, with no known duration, one that never completed).
        let skipped = if p.duration_ms > 0 { p.listened_ms * 2 < p.duration_ms } else { !p.completed };
        listened += p.listened_ms;
        if skipped {
            skips += 1;
        }
        if seen_tracks.insert(&p.sha256) && first_play.get(p.sha256.as_str()).copied().unwrap_or(p.started_at) >= from_ms {
            discoveries += 1;
        }
        if !p.artist.trim().is_empty() {
            seen_artists.insert(p.artist.to_lowercase());
        }
        if !skipped {
            let artist = if p.artist.trim().is_empty() { "Unknown artist".to_string() } else { p.artist.clone() };
            let a = artists.entry(artist.clone()).or_default();
            a.plays += 1;
            a.ms += p.listened_ms;
            let t = tracks.entry(p.sha256.clone()).or_insert_with(|| Acc { name: Some(p.title.clone()), sub: p.artist.clone(), ..Default::default() });
            t.plays += 1;
            t.ms += p.listened_ms;
            if !p.album.trim().is_empty() {
                let al = albums.entry(format!("{}\u{0}{}", p.album, p.artist)).or_insert_with(|| Acc { name: Some(p.album.clone()), sub: p.artist.clone(), ..Default::default() });
                al.plays += 1;
                al.ms += p.listened_ms;
            }
            for g in &p.genres {
                let e = genres.entry(g.clone()).or_default();
                e.plays += 1;
                e.ms += p.listened_ms;
            }
        }
    }

    let days: Vec<i64> = by_day.keys().copied().collect();
    let (longest, current) = streaks(&days, epoch_day(now_ms).0);
    let peak = by_day.iter().max_by_key(|(_, v)| **v).map(|(k, v)| (*k, *v));

    let top_of = |m: HashMap<String, Acc>| -> Vec<Entry> {
        let mut v: Vec<(String, Acc)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.plays.cmp(&a.1.plays).then(b.1.ms.cmp(&a.1.ms)).then(a.0.cmp(&b.0)));
        v.into_iter().take(top).map(|(k, a)| Entry { name: a.name.unwrap_or_else(|| k.split('\u{0}').next().unwrap_or("").to_string()), plays: a.plays, listened_ms: a.ms, sub: a.sub }).collect()
    };

    let n = rows.len() as i64;
    Summary {
        from_ms,
        to_ms,
        plays: n,
        skips,
        listened_ms: listened,
        unique_tracks: seen_tracks.len() as i64,
        unique_artists: seen_artists.len() as i64,
        discoveries,
        active_days: days.len() as i64,
        longest_streak_days: longest,
        current_streak_days: current,
        by_hour,
        by_weekday,
        by_day: by_day.iter().map(|(k, v)| [*k, *v]).collect(),
        top_artists: top_of(artists),
        top_tracks: top_of(tracks),
        top_albums: top_of(albums),
        top_genres: top_of(genres),
        peak_day: peak.map(|p| p.0),
        peak_day_plays: peak.map(|p| p.1).unwrap_or(0),
        skip_rate: if n == 0 { 0.0 } else { skips as f32 / n as f32 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sha: &str, at: i64, completed: bool, artist: &str, album: &str, genres: &[&str]) -> PlayRow {
        PlayRow { sha256: sha.into(), started_at: at, listened_ms: if completed { 240_000 } else { 60_000 }, completed, title: format!("Song {sha}"), artist: artist.into(), album: album.into(), duration_ms: 240_000, genres: genres.iter().map(|g| g.to_string()).collect() }
    }

    #[test]
    fn summary_counts_and_tops() {
        let day = 86_400_000i64;
        let base = 1_772_000_000_000i64; // a fixed point in 2026
        let rows = vec![
            row("a", base, true, "Daft Punk", "Discovery", &["house"]),
            row("a", base + day, true, "Daft Punk", "Discovery", &["house"]),
            row("b", base + day + 3_600_000, true, "Daft Punk", "Discovery", &["house"]),
            row("c", base + 2 * day, true, "Portishead", "", &["trip-hop"]),
            row("d", base + 2 * day, false, "Portishead", "", &[]),
            row("a", base - 20 * day, true, "Daft Punk", "", &[]), // earlier: a is a repeat
        ];
        let s = summarise(&rows, base - day, base + 10 * day, base + 3 * day, 10);
        assert_eq!(s.plays, 5);
        assert_eq!(s.skips, 1);
        assert_eq!(s.unique_tracks, 4);
        assert_eq!(s.unique_artists, 2);
        assert_eq!(s.discoveries, 3);
        assert_eq!(s.listened_ms, 4 * 240_000 + 60_000);
        assert_eq!(s.active_days, 3);
        assert_eq!(s.longest_streak_days, 3);
        assert_eq!(s.current_streak_days, 3);
        assert_eq!(s.top_artists[0].name, "Daft Punk");
        assert_eq!(s.top_artists[0].plays, 3);
        assert_eq!(s.top_tracks[0].name, "Song a");
        assert_eq!(s.top_albums[0].name, "Discovery");
        assert_eq!(s.top_genres[0].name, "house");
        assert_eq!(s.peak_day_plays, 2);
        assert!((s.skip_rate - 0.2).abs() < 1e-6);
        assert_eq!(s.by_hour.iter().sum::<i64>(), 5);
        assert_eq!(s.top_artists.iter().find(|e| e.name == "Portishead").unwrap().plays, 1);
    }

    #[test]
    fn streak_rules() {
        assert_eq!(streaks(&[10, 11, 12], 12), (3, 3));
        assert_eq!(streaks(&[10, 11, 12], 13), (3, 3));
        assert_eq!(streaks(&[10, 11, 12], 14), (3, 0));
        assert_eq!(streaks(&[1, 2, 5], 5), (2, 1));
        assert_eq!(streaks(&[], 5), (0, 0));
    }

    #[test]
    fn periods_are_ordered() {
        let now = 1_772_000_000_000i64;
        for p in ["month", "last-month", "year", "7d", "30d", "all"] {
            let (a, b) = period(p, now);
            assert!(a < b, "{p}");
        }
        let (a, b) = period("7d", now);
        assert_eq!((b - a) / 86_400_000, 7);
        let (a, b) = period("month", now);
        assert!(a <= now && now < b);
    }
}
