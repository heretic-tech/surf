//! Request / response hooks, blocking, interception, proxy auth.
//!
//! `Network.enable` / `Fetch.enable` are held by `DomainGuard`s owned by the
//! registered hooks; when the last hook is removed the domain is disabled.
//! Implemented in task 9.

/// A URL pattern (`*` glob) used by `on request(pattern)` and friends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPattern(pub String);

impl UrlPattern {
    /// Whether `url` matches (glob `*` wildcards).
    pub fn matches(&self, url: &str) -> bool {
        glob_match(&self.0, url)
    }
}

fn glob_match(pat: &str, s: &str) -> bool {
    let mut parts = pat.split('*');
    let first = parts.next().unwrap_or("");
    if !s.starts_with(first) {
        return false;
    }
    let mut rest = &s[first.len()..];
    let mut last_part: Option<&str> = None;
    for p in parts {
        last_part = Some(p);
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    match last_part {
        None => rest.is_empty(),
        Some(p) => p.is_empty() || s.ends_with(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(UrlPattern("*/api/*".into()).matches("https://x.com/api/v1"));
        assert!(UrlPattern("https://x.com/*".into()).matches("https://x.com/"));
        assert!(!UrlPattern("https://y.com/*".into()).matches("https://x.com/"));
        assert!(UrlPattern("*.png".into()).matches("https://x.com/a.png"));
        assert!(!UrlPattern("*.png".into()).matches("https://x.com/a.jpg"));
    }
}
