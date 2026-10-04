//! Subcommand implementations.

use std::path::PathBuf;
use surf_runtime::{RunError, RuntimeOptions};

/// Exit code for syntax errors.
pub const EXIT_SYNTAX: i32 = 2;
/// Exit code for runtime errors.
pub const EXIT_RUNTIME: i32 = 1;
/// Exit code when no browser can be found.
pub const EXIT_NO_BROWSER: i32 = 3;

fn read_source(file: &PathBuf) -> Result<(String, String), i32> {
    if file.as_os_str() == "-" {
        let mut s = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s) {
            eprintln!("surf: cannot read stdin: {e}");
            return Err(EXIT_RUNTIME);
        }
        return Ok(("<stdin>".to_string(), s));
    }
    match std::fs::read_to_string(file) {
        Ok(s) => Ok((file.display().to_string(), s)),
        Err(e) => {
            eprintln!("surf: cannot read {}: {e}", file.display());
            Err(EXIT_RUNTIME)
        }
    }
}

/// `surf run`.
pub async fn run(file: PathBuf, chrome: Option<PathBuf>, trace_cdp: bool, headless: bool) -> i32 {
    let (name, source) = match read_source(&file) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let opts = RuntimeOptions {
        trace_cdp,
        chrome_path: chrome,
        force_headless: headless,
    };
    match surf_runtime::run_source(&name, &source, opts).await {
        Ok(code) => code,
        Err(RunError::Syntax(d)) => {
            eprint!("{}", d.render(&source, use_color()));
            EXIT_SYNTAX
        }
        Err(RunError::Runtime(e)) => {
            eprintln!("error: {e}");
            if e.message.contains("no Chrome/Chromium found") {
                EXIT_NO_BROWSER
            } else {
                EXIT_RUNTIME
            }
        }
    }
}

/// `surf check`.
pub async fn check(file: PathBuf) -> i32 {
    let (name, source) = match read_source(&file) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let program = match surf_syntax::parse(&name, &source) {
        Ok(p) => p,
        Err(d) => {
            eprint!("{}", d.render(&source, use_color()));
            return EXIT_SYNTAX;
        }
    };
    if let Err(e) = surf_vm::compile(&name, &program) {
        eprint!("{}", e.into_diagnostics(&name).render(&source, use_color()));
        return EXIT_SYNTAX;
    }
    println!("{name}: ok");
    0
}

/// `surf doctor` (task 10 fills this in).
pub async fn doctor(chrome: Option<PathBuf>) -> i32 {
    let found = chrome.or_else(surf_browser::find_chrome);
    match found {
        Some(p) => {
            println!("chrome: {}", p.display());
            0
        }
        None => {
            println!("chrome: not found (set SURF_CHROME or `browser:\n    path: …`)");
            EXIT_NO_BROWSER
        }
    }
}

/// `surf repl` (task 10).
pub async fn repl() -> i32 {
    eprintln!("surf repl: not implemented yet");
    EXIT_RUNTIME
}

fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stderr())
}
