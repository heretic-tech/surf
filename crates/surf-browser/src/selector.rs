//! Selector syntax: CSS by default, `text=…`, `xpath=…` or a leading `//`.
//!
//! Resolution happens in the page's isolated world through one JS helper,
//! [`RESOLVER_SOURCE`], installed on the world's global object under a
//! random name (see [`crate::world::World`]). The main world never sees
//! it. `text=` matches the trimmed `textContent` of the *deepest* element:
//! exact (case-insensitive) matches win over substring matches; `script`,
//! `style`, `noscript` and `template` subtrees are ignored.

/// JavaScript source of the selector resolver: `(kind, sel, all) =>
/// Element | Element[] | null`. `kind` is `css`, `xpath` or `text`.
pub const RESOLVER_SOURCE: &str = r#"function(kind, sel, all) {
  const doc = document;
  if (kind === 'css') {
    return all ? Array.from(doc.querySelectorAll(sel)) : doc.querySelector(sel);
  }
  if (kind === 'xpath') {
    const r = doc.evaluate(sel, doc, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
    const out = [];
    for (let i = 0; i < r.snapshotLength; i++) {
      const n = r.snapshotItem(i);
      if (n && n.nodeType === 1) out.push(n);
    }
    return all ? out : (out[0] || null);
  }
  if (kind === 'text') {
    const needle = sel.trim().toLowerCase();
    const skip = new Set(['script', 'style', 'noscript', 'template']);
    const exact = [], contains = [];
    const visit = (el) => {
      if (skip.has(el.localName)) return ['', false];
      let text = '', childMatched = false;
      for (const n of el.childNodes) {
        if (n.nodeType === 3) text += n.data;
        else if (n.nodeType === 1) {
          const [t, m] = visit(n);
          text += t;
          childMatched = childMatched || m;
        }
      }
      let matched = false;
      if (!childMatched && needle) {
        const t = text.trim().toLowerCase();
        if (t === needle) { exact.push(el); matched = true; }
        else if (t.includes(needle)) { contains.push(el); matched = true; }
      }
      return [text, matched || childMatched];
    };
    if (doc.body) visit(doc.body);
    const picked = exact.length ? exact : contains;
    return all ? picked : (picked[0] || null);
  }
  throw new Error('unknown selector kind ' + kind);
}"#;

/// A parsed selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// CSS selector.
    Css(String),
    /// Exact / substring text match (`text=Buy now`).
    Text(String),
    /// XPath (`xpath=//a` or `//a`).
    XPath(String),
}

impl Selector {
    /// Parse the surface syntax.
    pub fn parse(s: &str) -> Selector {
        if let Some(rest) = s.strip_prefix("text=") {
            Selector::Text(rest.to_string())
        } else if let Some(rest) = s.strip_prefix("xpath=") {
            Selector::XPath(rest.to_string())
        } else if s.starts_with("//") || s.starts_with("(//") {
            Selector::XPath(s.to_string())
        } else {
            Selector::Css(s.to_string())
        }
    }

    /// The original user-facing text (for errors).
    pub fn source(&self) -> String {
        match self {
            Selector::Css(s) => s.clone(),
            Selector::Text(s) => format!("text={s}"),
            Selector::XPath(s) => format!("xpath={s}"),
        }
    }

    /// `(kind, selector)` as the resolver expects them.
    pub fn parts(&self) -> (&'static str, &str) {
        match self {
            Selector::Css(s) => ("css", s),
            Selector::Text(s) => ("text", s),
            Selector::XPath(s) => ("xpath", s),
        }
    }
}

impl From<&str> for Selector {
    fn from(s: &str) -> Selector {
        Selector::parse(s)
    }
}

impl From<String> for Selector {
    fn from(s: String) -> Selector {
        Selector::parse(&s)
    }
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.source())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixes() {
        assert_eq!(Selector::parse("#a"), Selector::Css("#a".into()));
        assert_eq!(
            Selector::parse("text=Buy now"),
            Selector::Text("Buy now".into())
        );
        assert_eq!(Selector::parse("xpath=//a"), Selector::XPath("//a".into()));
        assert_eq!(Selector::parse("//a"), Selector::XPath("//a".into()));
    }
}
