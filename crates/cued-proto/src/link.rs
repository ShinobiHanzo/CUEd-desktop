//! Query-string grammar shared by every `cued://` link, and the one entry
//! point that tells them apart.

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::{pair::PairLink, share::SharePayload, station::StationLink, Error, Result};

/// `URLEncoder.encode(s, "UTF-8").replace("+", "%20")` on the phone keeps
/// `-`, `_`, `.`, `*` and encodes everything else that is not alphanumeric.
const PHONE_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'*');

pub fn enc(s: &str) -> String {
    utf8_percent_encode(s, PHONE_SET).to_string()
}

pub fn dec(s: &str) -> String {
    // Java's URLDecoder turns `+` into a space; the phone never emits `+`
    // but accepts it, so we do too.
    percent_decode_str(&s.replace('+', "%20")).decode_utf8_lossy().into_owned()
}

/// Splits `k=v&k=v` into pairs in order; a pair without `=` is skipped.
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|pair| {
            let i = pair.find('=')?;
            if i == 0 {
                return None;
            }
            Some((pair[..i].to_string(), dec(&pair[i + 1..])))
        })
        .collect()
}

pub fn build_query(pairs: &[(&str, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&")
}

pub(crate) fn get<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Any link a QR code, NFC tag or paste box can hand us.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    Share(SharePayload),
    Station(StationLink),
    Pair(PairLink),
}

/// Recognises every CUEd link form, including the web share form.
pub fn decode_link(text: &str) -> Result<Link> {
    let t = text.trim();
    if let Ok(p) = PairLink::decode(t) {
        return Ok(Link::Pair(p));
    }
    if let Ok(s) = StationLink::decode(t) {
        return Ok(Link::Station(s));
    }
    SharePayload::decode(t).map(Link::Share).map_err(|_| Error::NotALink("cued"))
}

pub(crate) fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}
