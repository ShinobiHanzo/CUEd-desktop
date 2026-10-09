//! NIP-01 filters: parsed once per `REQ`, used both for the SQL query and for
//! matching live events against open subscriptions.

use std::collections::HashSet;

use cued_proto::Event;
use serde_json::Value;

use crate::{DEFAULT_LIMIT, MAX_LIMIT};

/// One filter object of a `REQ`. Several filters in one `REQ` are ORed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub ids: Option<HashSet<String>>,
    pub authors: Option<HashSet<String>>,
    pub kinds: Option<HashSet<u32>>,
    /// `#<name>` conditions: the event must carry a tag `name` whose value is in the set.
    pub tags: Vec<(String, HashSet<String>)>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// Already clamped to `MAX_LIMIT`; `DEFAULT_LIMIT` when absent.
    pub limit: usize,
}

fn string_set(key: &str, v: &Value) -> Result<HashSet<String>, String> {
    let arr = v.as_array().ok_or_else(|| format!("`{key}` must be an array of strings"))?;
    arr.iter()
        .map(|s| s.as_str().map(str::to_owned).ok_or_else(|| format!("`{key}` must be an array of strings")))
        .collect()
}

impl Filter {
    /// Parses one filter object. Unknown keys are ignored, as NIP-01 asks.
    pub fn parse(v: &Value) -> Result<Filter, String> {
        let obj = v.as_object().ok_or_else(|| "filter must be an object".to_string())?;
        let mut f = Filter { limit: DEFAULT_LIMIT, ..Filter::default() };
        for (k, v) in obj {
            match k.as_str() {
                "ids" => f.ids = Some(string_set(k, v)?),
                "authors" => f.authors = Some(string_set(k, v)?),
                "kinds" => {
                    let arr = v.as_array().ok_or_else(|| "`kinds` must be an array of integers".to_string())?;
                    let kinds = arr
                        .iter()
                        .map(|n| n.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| "`kinds` must be an array of integers".to_string()))
                        .collect::<Result<HashSet<u32>, String>>()?;
                    f.kinds = Some(kinds);
                }
                "since" => f.since = Some(v.as_i64().ok_or_else(|| "`since` must be an integer".to_string())?),
                "until" => f.until = Some(v.as_i64().ok_or_else(|| "`until` must be an integer".to_string())?),
                "limit" => {
                    let n = v.as_u64().ok_or_else(|| "`limit` must be a non-negative integer".to_string())?;
                    f.limit = usize::try_from(n).unwrap_or(MAX_LIMIT).min(MAX_LIMIT);
                }
                _ if k.starts_with('#') && k.len() > 1 => {
                    f.tags.push((k[1..].to_string(), string_set(k, v)?));
                }
                _ => {}
            }
        }
        Ok(f)
    }

    /// Parses every filter of a `REQ`; the first bad one fails the whole request.
    pub fn parse_all(filters: &[Value]) -> Result<Vec<Filter>, String> {
        filters.iter().map(Filter::parse).collect()
    }

    /// True when some set in the filter is empty, so nothing can ever match.
    pub fn is_empty_match(&self) -> bool {
        self.ids.as_ref().is_some_and(HashSet::is_empty)
            || self.authors.as_ref().is_some_and(HashSet::is_empty)
            || self.kinds.as_ref().is_some_and(HashSet::is_empty)
            || self.tags.iter().any(|(_, vs)| vs.is_empty())
    }

    /// Does this event satisfy the filter? (`limit` plays no part here.)
    pub fn matches(&self, ev: &Event) -> bool {
        if let Some(ids) = &self.ids {
            if !ids.contains(&ev.id) {
                return false;
            }
        }
        if let Some(authors) = &self.authors {
            if !authors.contains(&ev.pubkey) {
                return false;
            }
        }
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&ev.kind) {
                return false;
            }
        }
        if self.since.is_some_and(|s| ev.created_at < s) || self.until.is_some_and(|u| ev.created_at > u) {
            return false;
        }
        self.tags.iter().all(|(name, values)| {
            ev.tags.iter().any(|t| t.len() >= 2 && t[0] == *name && values.contains(&t[1]))
        })
    }
}

/// True when any filter matches.
pub fn any_matches(filters: &[Filter], ev: &Event) -> bool {
    filters.iter().any(|f| f.matches(ev))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: u32, tags: Vec<Vec<String>>, created_at: i64) -> Event {
        Event { id: "i".into(), pubkey: "p".into(), created_at, kind, tags, content: String::new(), sig: "s".into() }
    }

    #[test]
    fn parses_and_matches() {
        let f = Filter::parse(&json!({"kinds":[1,30777],"#d":["cued"],"since":10,"until":20,"limit":5000,"search":"x"})).unwrap();
        assert_eq!(f.limit, MAX_LIMIT);
        assert!(f.matches(&ev(30777, vec![vec!["d".into(), "cued".into()]], 15)));
        assert!(!f.matches(&ev(30777, vec![vec!["d".into(), "other".into()]], 15)));
        assert!(!f.matches(&ev(30777, vec![vec!["d".into(), "cued".into()]], 25)));
        assert!(!f.matches(&ev(2, vec![vec!["d".into(), "cued".into()]], 15)));
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(Filter::parse(&json!([])).is_err());
        assert!(Filter::parse(&json!({"kinds":"1"})).is_err());
        assert!(Filter::parse(&json!({"authors":[1]})).is_err());
        assert!(Filter::parse(&json!({"since":"now"})).is_err());
    }

    #[test]
    fn empty_sets_match_nothing() {
        let f = Filter::parse(&json!({"ids":[]})).unwrap();
        assert!(f.is_empty_match());
        assert!(!f.matches(&ev(1, vec![], 1)));
        let f = Filter::parse(&json!({})).unwrap();
        assert!(!f.is_empty_match());
        assert_eq!(f.limit, DEFAULT_LIMIT);
        assert!(f.matches(&ev(1, vec![], 1)));
    }
}
