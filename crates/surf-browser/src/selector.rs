//! Selector syntax: CSS by default, `text=…`, `xpath=…` or a leading `//`.

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
