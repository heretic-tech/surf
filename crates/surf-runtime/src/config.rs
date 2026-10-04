//! `browser:` block → launch options.
//!
//! Props arrive already evaluated (`Declaration::Config`). Values that do
//! not type-check are recorded as `problems` and raised as a runtime error
//! the first time the browser is needed — never at declaration time, so a
//! script that never touches the browser still runs. Unknown keys are a
//! `surf check` error and never reach here.

use crate::host::RuntimeOptions;
use indexmap::IndexMap;
use std::rc::Rc;
use std::time::Duration;
use surf_browser::{CdpMode, LaunchOptions};
use surf_vm::{RuntimeError, Value};

/// Declarative browser configuration. Nothing launches until the first
/// action that needs a page.
#[derive(Debug, Clone, Default)]
pub struct BrowserConfig {
    /// Resolved launch options.
    pub launch: LaunchOptions,
    /// `pool: "wss://…"` remote provider URL (Decision 8).
    pub pool: Option<String>,
    /// Values that could not be understood, reported at first use.
    pub problems: Vec<String>,
}

impl BrowserConfig {
    /// Build from evaluated `key: value` props.
    pub fn from_props(props: &IndexMap<Rc<str>, Value>) -> BrowserConfig {
        let mut cfg = BrowserConfig::default();
        let launch = &mut cfg.launch;
        let mut bad = |key: &str, v: &Value, expected: &str| {
            cfg.problems.push(format!(
                "browser.{key}: expected {expected}, got {}",
                describe(v)
            ));
        };
        for (k, v) in props {
            match (&**k, v) {
                ("path", Value::Str(s)) => launch.path = Some(s.to_string().into()),
                ("path", v) => bad("path", v, "a string"),
                ("cdp", Value::Int(port)) if (1..=65535).contains(port) => {
                    launch.cdp = CdpMode::Port(*port as u16)
                }
                ("cdp", Value::Int(0)) => launch.cdp = CdpMode::Port(0),
                ("cdp", Value::Str(s)) if &**s == "pipe" => launch.cdp = CdpMode::Pipe,
                ("cdp", Value::Str(s)) if s.starts_with("ws://") || s.starts_with("wss://") => {
                    launch.cdp = CdpMode::Attach(s.to_string())
                }
                ("cdp", v) => bad("cdp", v, "pipe, a port number or a ws:// URL"),
                ("pool", Value::Str(s)) => cfg.pool = Some(s.to_string()),
                ("pool", Value::Nil) => {}
                ("pool", v) => bad("pool", v, "a wss:// URL"),
                ("proxy", Value::Str(s)) => launch.proxy = Some(s.to_string()),
                ("proxy", Value::Nil) => {}
                ("proxy", v) => bad("proxy", v, "a proxy URL"),
                ("proxies", Value::List(l)) => {
                    launch.proxies = l.borrow().iter().map(|v| v.to_string()).collect()
                }
                ("proxies", v) => bad("proxies", v, "a list of proxy URLs"),
                ("headless", Value::Bool(b)) => launch.headless = Some(*b),
                ("headless", v) => bad("headless", v, "true or false"),
                ("virtual", Value::Bool(b)) => launch.virtual_display = *b,
                ("virtual", v) => bad("virtual", v, "true or false"),
                ("size", Value::Str(s)) => match parse_size(s) {
                    Some(size) => launch.size = size,
                    None => bad("size", v, "\"WIDTHxHEIGHT\" such as \"1280x800\""),
                },
                ("size", v) => bad("size", v, "\"WIDTHxHEIGHT\" such as \"1280x800\""),
                ("profile", Value::Str(s)) => launch.profile = Some(s.to_string().into()),
                ("profile", v) => bad("profile", v, "a directory path"),
                ("flags", Value::List(l)) => {
                    launch.flags = l.borrow().iter().map(|v| v.to_string()).collect()
                }
                ("flags", v) => bad("flags", v, "a list of strings"),
                ("timeout", Value::Duration(d)) => launch.timeout = *d,
                ("timeout", v) => bad("timeout", v, "a duration such as 30s"),
                ("engine", Value::Str(s)) => launch.engine = s.to_string(),
                ("engine", v) => bad("engine", v, "chrome"),
                _ => {}
            }
        }
        cfg
    }

    /// CLI flags win over the script: `--chrome` fills in `path`,
    /// `--headless` / `--headed` force the mode, `--timeout` sets the
    /// default action timeout.
    pub fn apply_overrides(&mut self, opts: &RuntimeOptions) {
        if let Some(p) = &opts.chrome_path {
            self.launch.path = Some(p.clone());
        }
        if let Some(h) = opts.headless {
            self.launch.headless = Some(h);
        }
        if let Some(t) = opts.timeout {
            self.launch.timeout = t;
        }
    }

    /// Validate before launching: recorded value problems, the engine
    /// (`apostate` is reserved — Decision 7), attach-mode conflicts.
    pub fn check(&self, alias: &str) -> Result<(), RuntimeError> {
        let block = if alias == "default" {
            "browser:".to_string()
        } else {
            format!("browser {alias}:")
        };
        if let Some(p) = self.problems.first() {
            return Err(RuntimeError::new(format!("{block} {p}")));
        }
        match self.launch.engine.as_str() {
            "chrome" => {}
            "apostate" => {
                return Err(RuntimeError::new(format!(
                    "{block} engine: apostate is not available yet — see TASKS.md \
                     (\"Engine / fingerprinting\"); use engine: chrome"
                )))
            }
            other => {
                return Err(RuntimeError::new(format!(
                    "{block} unknown engine {other:?} (expected chrome)"
                )))
            }
        }
        if self.pool.is_some() && matches!(self.launch.cdp, CdpMode::Attach(_)) {
            return Err(RuntimeError::new(format!(
                "{block} pool: and cdp: \"ws://…\" both attach to a remote browser — keep one"
            )));
        }
        Ok(())
    }

    /// The default action timeout.
    pub fn timeout(&self) -> Duration {
        self.launch.timeout
    }
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

fn describe(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("{s:?}"),
        other => format!("{other} ({})", other.type_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, Value)]) -> IndexMap<Rc<str>, Value> {
        pairs
            .iter()
            .map(|(k, v)| (Rc::from(*k), v.clone()))
            .collect()
    }

    #[test]
    fn parses_every_key() {
        let cfg = BrowserConfig::from_props(&props(&[
            ("path", Value::str("/x/chrome")),
            ("cdp", Value::Int(9222)),
            ("pool", Value::str("wss://pool")),
            ("proxy", Value::str("http://u:p@h:1")),
            ("proxies", Value::list(vec![Value::str("http://a:1")])),
            ("headless", Value::Bool(true)),
            ("virtual", Value::Bool(true)),
            ("size", Value::str("800x600")),
            ("profile", Value::str("./p")),
            ("flags", Value::list(vec![Value::str("--lang=en")])),
            ("timeout", Value::Duration(Duration::from_secs(5))),
            ("engine", Value::str("chrome")),
        ]));
        assert!(cfg.problems.is_empty(), "{:?}", cfg.problems);
        assert_eq!(cfg.launch.cdp, CdpMode::Port(9222));
        assert_eq!(cfg.pool.as_deref(), Some("wss://pool"));
        assert_eq!(cfg.launch.size, (800, 600));
        assert_eq!(cfg.launch.flags, vec!["--lang=en".to_string()]);
        assert_eq!(cfg.launch.timeout, Duration::from_secs(5));
        assert!(cfg.launch.virtual_display);
        assert!(cfg.check("default").is_ok());
    }

    #[test]
    fn symbols_and_attach() {
        let cfg = BrowserConfig::from_props(&props(&[("cdp", Value::str("pipe"))]));
        assert_eq!(cfg.launch.cdp, CdpMode::Pipe);
        let cfg = BrowserConfig::from_props(&props(&[("cdp", Value::str("ws://h:1/devtools"))]));
        assert_eq!(cfg.launch.cdp, CdpMode::Attach("ws://h:1/devtools".into()));
    }

    #[test]
    fn apostate_is_a_clear_not_yet() {
        let cfg = BrowserConfig::from_props(&props(&[("engine", Value::str("apostate"))]));
        let msg = cfg.check("default").unwrap_err().message;
        assert!(msg.contains("not available yet"), "{msg}");
        assert!(msg.contains("TASKS.md"), "{msg}");
        let cfg = BrowserConfig::from_props(&props(&[("engine", Value::str("firefox"))]));
        assert!(cfg
            .check("work")
            .unwrap_err()
            .message
            .contains("browser work:"));
    }

    #[test]
    fn bad_values_are_reported_at_first_use() {
        let cfg = BrowserConfig::from_props(&props(&[
            ("size", Value::str("wide")),
            ("timeout", Value::Int(3)),
        ]));
        assert_eq!(cfg.problems.len(), 2);
        let msg = cfg.check("default").unwrap_err().message;
        assert!(msg.starts_with("browser: browser.size"), "{msg}");
    }

    #[test]
    fn cli_overrides_win() {
        let mut cfg = BrowserConfig::from_props(&props(&[("headless", Value::Bool(false))]));
        cfg.apply_overrides(&RuntimeOptions {
            headless: Some(true),
            timeout: Some(Duration::from_secs(7)),
            chrome_path: Some("/c".into()),
            ..Default::default()
        });
        assert_eq!(cfg.launch.headless, Some(true));
        assert_eq!(cfg.launch.timeout, Duration::from_secs(7));
        assert_eq!(cfg.launch.path.as_deref(), Some(std::path::Path::new("/c")));
    }
}
