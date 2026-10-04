//! `browser:` block → launch options.

use indexmap::IndexMap;
use std::rc::Rc;
use surf_browser::{CdpMode, LaunchOptions};
use surf_vm::Value;

/// Declarative browser configuration. Nothing launches until the first
/// action that needs a page.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Resolved launch options.
    pub launch: LaunchOptions,
    /// `pool: "wss://…"` remote provider URL (Decision 8).
    pub pool: Option<String>,
}

impl BrowserConfig {
    /// Build from evaluated `key: value` props. Unknown keys are reported
    /// by `surf check` (task 7); here they are ignored.
    pub fn from_props(props: &IndexMap<Rc<str>, Value>) -> BrowserConfig {
        let mut launch = LaunchOptions::default();
        let mut pool = None;
        for (k, v) in props {
            match (&**k, v) {
                ("path", Value::Str(s)) => launch.path = Some(s.to_string().into()),
                ("cdp", Value::Int(port)) => launch.cdp = CdpMode::Port(*port as u16),
                ("cdp", Value::Str(s)) if s.starts_with("ws") => {
                    launch.cdp = CdpMode::Attach(s.to_string())
                }
                ("pool", Value::Str(s)) => pool = Some(s.to_string()),
                ("proxy", Value::Str(s)) => launch.proxy = Some(s.to_string()),
                ("proxies", Value::List(l)) => {
                    launch.proxies = l.borrow().iter().map(|v| v.to_string()).collect()
                }
                ("headless", Value::Bool(b)) => launch.headless = Some(*b),
                ("virtual", Value::Bool(b)) => launch.virtual_display = *b,
                ("size", Value::Str(s)) => {
                    if let Some((w, h)) = s.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                            launch.size = (w, h);
                        }
                    }
                }
                ("profile", Value::Str(s)) => launch.profile = Some(s.to_string().into()),
                ("flags", Value::List(l)) => {
                    launch.flags = l.borrow().iter().map(|v| v.to_string()).collect()
                }
                ("timeout", Value::Duration(d)) => launch.timeout = *d,
                ("engine", Value::Str(s)) => launch.engine = s.to_string(),
                _ => {}
            }
        }
        BrowserConfig { launch, pool }
    }
}
