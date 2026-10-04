//! # surf
//!
//! The command-line entry point.
//!
//! - `surf run <file.surf> [--json] [--trace-cdp] [--timeout 30s]
//!   [--headless|--headed] [--chrome <path>]` — parse, compile, run (tokio
//!   `current_thread` + `LocalSet`). `surf <file.surf>` is the shorthand,
//!   so `#!/usr/bin/env surf` works as a shebang.
//! - `surf check <file.surf>` — parse + compile only; print diagnostics.
//! - `surf doctor` — find Chrome, launch it quietly over the pipe, report
//!   versions, timings and the exact flags used.
//! - `surf repl` — line-at-a-time session; browser and pages stay open.
//! - `surf install` — download Chrome for Testing (stable) into
//!   `~/.cache/surf/chrome/<version>/`.
//!
//! Exit codes: 0 ok, 1 runtime error, 2 syntax error, 3 no browser, 130
//! interrupted; `exit(n)` in a script sets `n`.

#![forbid(unsafe_code)]

mod commands;

use clap::{Args, Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

/// Quiet Chromium scripting.
#[derive(Parser, Debug)]
#[command(name = "surf", version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    /// Log every CDP frame (both directions) to stderr.
    #[arg(long, global = true)]
    trace_cdp: bool,

    /// Chrome binary (overrides `browser.path` and SURF_CHROME).
    #[arg(long, global = true, env = "SURF_CHROME", value_name = "PATH")]
    chrome: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

/// Options shared by `run` and `repl`.
#[derive(Args, Debug, Clone, Default)]
struct RunOpts {
    /// Make `print` write JSON lines too (`{"print": "…"}`), so stdout is
    /// one JSON document per line next to `emit`.
    #[arg(long)]
    json: bool,

    /// Default action timeout (`30s`, `500ms`, `2m`); overrides `timeout:`.
    #[arg(long, value_name = "DURATION", value_parser = commands::parse_duration)]
    timeout: Option<std::time::Duration>,

    /// Run headless even if a display exists.
    #[arg(long, conflicts_with = "headed")]
    headless: bool,

    /// Run headed even if the script says `headless: true`.
    #[arg(long, conflicts_with = "headless")]
    headed: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a script.
    Run {
        /// Script path (`-` for stdin).
        file: PathBuf,
        #[command(flatten)]
        opts: RunOpts,
    },
    /// Parse and compile a script without running it.
    Check {
        /// Script path (`-` for stdin).
        file: PathBuf,
    },
    /// Diagnose the local Chrome installation (discovery, display, a timed
    /// pipe launch).
    Doctor,
    /// Interactive session: one line (or indented block) at a time; the
    /// browser and its pages stay open between lines. `.exit` ends it.
    Repl {
        #[command(flatten)]
        opts: RunOpts,
    },
    /// Download Chrome for Testing (stable) into ~/.cache/surf/chrome.
    Install {
        /// Re-download even if this version is already installed.
        #[arg(long)]
        force: bool,
    },
}

const SUBCOMMANDS: &[&str] = &["run", "check", "doctor", "repl", "install", "help"];

/// `surf script.surf …` → `surf run script.surf …` when the first argument
/// is not a subcommand or flag and names an existing file (or ends in
/// `.surf`).
fn normalise_args() -> Vec<OsString> {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if let Some(first) = args.get(1) {
        let s = first.to_string_lossy();
        let is_sub = SUBCOMMANDS.contains(&&*s);
        let looks_like_script = !s.starts_with('-')
            && (s.ends_with(".surf") || s == "-" || PathBuf::from(&*s).is_file());
        if !is_sub && looks_like_script {
            args.insert(1, OsString::from("run"));
        }
    }
    args
}

fn main() {
    let cli = Cli::parse_from(normalise_args());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    if cli.trace_cdp {
        // Picked up by surf-cdp for frames sent before the runtime sees the
        // connection (launch handshake).
        std::env::set_var("SURF_TRACE_CDP", "1");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    let code = local.block_on(&rt, async move {
        match cli.command {
            Command::Run { file, opts } => {
                commands::run(file, commands::options(&opts, cli.chrome, cli.trace_cdp)).await
            }
            Command::Check { file } => commands::check(file).await,
            Command::Doctor => commands::doctor(cli.chrome).await,
            Command::Repl { opts } => {
                commands::repl(commands::options(&opts, cli.chrome, cli.trace_cdp)).await
            }
            Command::Install { force } => commands::install(force).await,
        }
    });
    // Let the LocalSet drop (aborts leftover observer tasks) before exiting.
    drop(local);
    std::process::exit(code);
}
