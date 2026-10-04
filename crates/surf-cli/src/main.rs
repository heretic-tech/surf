//! # surf
//!
//! The command-line entry point.
//!
//! - `surf run <file.surf>` — parse, compile, run (tokio `current_thread` +
//!   `LocalSet`).
//! - `surf check <file.surf>` — parse + compile only; print diagnostics.
//! - `surf doctor` — find Chrome, launch it quietly, report versions and
//!   the exact flags used (task 10).
//! - `surf repl` — interactive session against one page (task 10).
//!
//! Exit codes: 0 ok, 1 runtime error, 2 syntax error, 3 no browser; `exit(n)`
//! in a script sets `n`.

#![forbid(unsafe_code)]

mod commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Quiet Chromium scripting.
#[derive(Parser, Debug)]
#[command(name = "surf", version, about)]
struct Cli {
    /// Log every CDP frame (both directions) to stderr.
    #[arg(long, global = true)]
    trace_cdp: bool,

    /// Chrome binary (overrides `browser.path` and SURF_CHROME).
    #[arg(long, global = true, env = "SURF_CHROME")]
    chrome: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a script.
    Run {
        /// Script path (`-` for stdin).
        file: PathBuf,
        /// Force headless even if a display exists.
        #[arg(long)]
        headless: bool,
    },
    /// Parse and compile a script without running it.
    Check {
        /// Script path.
        file: PathBuf,
    },
    /// Diagnose the local Chrome installation.
    Doctor,
    /// Interactive session.
    Repl,
}

fn main() {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    let code = local.block_on(&rt, async move {
        match cli.command {
            Command::Run { file, headless } => {
                commands::run(file, cli.chrome, cli.trace_cdp, headless).await
            }
            Command::Check { file } => commands::check(file).await,
            Command::Doctor => commands::doctor(cli.chrome).await,
            Command::Repl => commands::repl().await,
        }
    });
    std::process::exit(code);
}
