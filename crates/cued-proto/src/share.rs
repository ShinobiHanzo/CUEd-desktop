//! `cued://share?…`: one track handed from one device to another. Mirrors
//! `core/share/SharePayload.kt` on the phone.

use crate::link::{build_query, get, parse_query};
use crate::{Error, Result, SCHEME, SHARE_PAYLOAD_VERSION, SHARE_WEB_BASE};

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct SharePayload {
    pub title: String,
    pub artist: String,
    /// Source URL usable by spotdl, if known.
    pub link: Option<String>,
    /// URL of the audio file on the sender's share server.
    pub file_url: Option<String>,
    /// URL to download the CUEd APK.
    pub apk_url: Option<String>,
    pub genres: Vec<String>,
    pub bpm: Option<f32>,
    pub version: u32,
}

impl SharePayload {
    pub fn new(title: impl Into<String>, artist: impl Into<String>) -> Self {
        Self { title: title.into(), artist: artist.into(), version: SHARE_PAYLOAD_VERSION, ..Default::default() }
    }

    fn prefix() -> String {
        format!("{SCHEME}://share?")
    }

    fn query(&self) -> String {
        let mut q: Vec<(&str, String)> = vec![("v", self.version.to_string()), ("t", self.title.clone()), ("a", self.artist.clone())];
        if let Some(l) = &self.link {
            q.push(("l", l.clone()));
        }
        if let Some(f) = &self.file_url {
            q.push(("f", f.clone()));
        }
        if let Some(k) = &self.apk_url {
            q.push(("k", k.clone()));
        }
        if !self.genres.is_empty() {
            q.push(("g", self.genres.join(",")));
        }
        if let Some(b) = self.bpm {
            q.push(("b", format!("{b:.1}")));
        }
        build_query(&q)
    }

    /// `cued://share?…`
    pub fn encode(&self) -> String {
        format!("{}{}", Self::prefix(), self.query())
    }

    /// Same fields behind the download page's URL; what QR and NFC carry.
    pub fn encode_web(&self) -> String {
        format!("{SHARE_WEB_BASE}#{}", self.query())
    }

    /// Accepts `cued://share?…` and the web form `…/CUEd/#…` (or `?…`).
    pub fn decode(text: &str) -> Result<Self> {
        let t = text.trim();
        let prefix = Self::prefix();
        let query = if let Some(rest) = t.strip_prefix(prefix.as_str()) {
            rest
        } else if t.len() >= SHARE_WEB_BASE.len() && t[..SHARE_WEB_BASE.len()].eq_ignore_ascii_case(SHARE_WEB_BASE) {
            t[SHARE_WEB_BASE.len()..].trim_start_matches(['#', '?'])
        } else {
            return Err(Error::NotALink("share"));
        };
        let q = parse_query(query);
        let title = get(&q, "t").ok_or(Error::Missing("t"))?.to_string();
        Ok(Self {
            title,
            artist: get(&q, "a").unwrap_or("").to_string(),
            link: get(&q, "l").map(str::to_string),
            file_url: get(&q, "f").map(str::to_string),
            apk_url: get(&q, "k").map(str::to_string),
            genres: get(&q, "g").map(|g| g.split(',').filter(|s| !s.trim().is_empty()).map(str::to_string).collect()).unwrap_or_default(),
            bpm: get(&q, "b").and_then(|b| b.parse().ok()),
            version: get(&q, "v").and_then(|v| v.parse().ok()).unwrap_or(1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_matches_phone_encoding() {
        let p = SharePayload {
            title: "Afterlife (Live) & more".into(),
            artist: "Evanescence".into(),
            link: Some("https://open.spotify.com/track/abc".into()),
            file_url: Some("http://192.168.1.5:8765/track/42".into()),
            apk_url: None,
            genres: vec!["rock".into(), "alt metal".into()],
            bpm: Some(128.04),
            version: 1,
        };
        let s = p.encode();
        assert_eq!(
            s,
            "cued://share?v=1&t=Afterlife%20%28Live%29%20%26%20more&a=Evanescence&l=https%3A%2F%2Fopen.spotify.com%2Ftrack%2Fabc&f=http%3A%2F%2F192.168.1.5%3A8765%2Ftrack%2F42&g=rock%2Calt%20metal&b=128.0"
        );
        let back = SharePayload::decode(&s).unwrap();
        assert_eq!(back.title, p.title);
        assert_eq!(back.genres, p.genres);
        assert_eq!(back.bpm, Some(128.0));
        let web = p.encode_web();
        assert!(web.starts_with("https://shinobihanzo.github.io/CUEd/#v=1&t="));
        assert_eq!(SharePayload::decode(&web).unwrap().artist, "Evanescence");
        assert!(SharePayload::decode("https://example.com/#t=x").is_err());
        assert!(SharePayload::decode("cued://share?a=only").is_err());
    }
}
