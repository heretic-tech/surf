//! Cookies via `Storage.getCookies` / `Storage.setCookies` /
//! `Storage.clearCookies` (browser-context scoped; no `Network.enable`
//! needed). [`Cookie`] is the script-facing shape; it converts to and from
//! the generated protocol structs.

use serde::{Deserialize, Serialize};
use surf_cdp::protocol::network;

/// A cookie as exchanged with CDP.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    /// Name.
    pub name: String,
    /// Value.
    pub value: String,
    /// Domain.
    #[serde(default)]
    pub domain: Option<String>,
    /// Path.
    #[serde(default)]
    pub path: Option<String>,
    /// Expiry (unix seconds); `-1` / `None` for session cookies.
    #[serde(default)]
    pub expires: Option<f64>,
    /// `HttpOnly`.
    #[serde(default)]
    pub http_only: Option<bool>,
    /// `Secure`.
    #[serde(default)]
    pub secure: Option<bool>,
    /// `SameSite` (`Strict`, `Lax`, `None`).
    #[serde(default)]
    pub same_site: Option<String>,
    /// URL to associate the cookie with when setting it (fills in domain,
    /// path, scheme). Never returned by Chrome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Cookie {
    /// `name=value` for `url` (the common case when setting).
    pub fn for_url(name: &str, value: &str, url: &str) -> Cookie {
        Cookie {
            name: name.into(),
            value: value.into(),
            url: Some(url.into()),
            ..Default::default()
        }
    }

    /// From Chrome's `Network.Cookie`.
    pub fn from_cdp(c: network::Cookie) -> Cookie {
        Cookie {
            name: c.name,
            value: c.value,
            domain: Some(c.domain),
            path: Some(c.path),
            expires: if c.session || c.expires < 0.0 {
                None
            } else {
                Some(c.expires)
            },
            http_only: Some(c.http_only),
            secure: Some(c.secure),
            same_site: c.same_site.and_then(same_site_name),
            url: None,
        }
    }

    /// To Chrome's `Network.CookieParam` (for `Storage.setCookies`).
    pub fn to_param(&self) -> network::CookieParam {
        network::CookieParam {
            name: self.name.clone(),
            value: self.value.clone(),
            url: self.url.clone(),
            domain: self.domain.clone(),
            path: self.path.clone(),
            secure: self.secure,
            http_only: self.http_only,
            same_site: self
                .same_site
                .as_deref()
                .and_then(|s| serde_json::from_value(serde_json::Value::String(s.into())).ok()),
            expires: self.expires.filter(|e| *e >= 0.0),
            ..Default::default()
        }
    }
}

fn same_site_name(v: network::CookieSameSite) -> Option<String> {
    match serde_json::to_value(v).ok()? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_same_site_and_session_expiry() {
        let c = Cookie {
            name: "a".into(),
            value: "b".into(),
            domain: Some("example.com".into()),
            path: Some("/".into()),
            expires: None,
            http_only: Some(true),
            secure: Some(false),
            same_site: Some("Lax".into()),
            url: None,
        };
        let p = c.to_param();
        assert_eq!(p.same_site, Some(network::CookieSameSite::Lax));
        assert_eq!(p.expires, None);
        let back: network::Cookie = serde_json::from_value(serde_json::json!({
            "name": "a", "value": "b", "domain": "example.com", "path": "/", "expires": -1,
            "size": 2, "httpOnly": true, "secure": false, "session": true, "sameSite": "Lax",
            "priority": "Medium", "sourceScheme": "Secure", "sourcePort": 443
        }))
        .unwrap();
        assert_eq!(Cookie::from_cdp(back), c);
    }
}
