mod actions;
mod blame;
mod cli;
mod headless;
mod keys;
mod search;
mod settings;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, InternalCommand};
use headless::{run_headless, HeadlessOptions};
use search::run_search_streaming;
use std::env;
use which::which;

fn ensure_dependency(binary: &str) -> Result<()> {
    which(binary).with_context(|| format!("required dependency not found in PATH: {binary}"))?;
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = env::current_dir().context("failed to read current working directory")?;

    match cli.internal {
        Some(InternalCommand::Search { query }) => {
            ensure_dependency("rg")?;
            run_search_streaming(&query, &cwd)?;
            return Ok(());
        }
        Some(InternalCommand::Copy { mode, path, line }) => {
            let base = if mode == "filename" {
                std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(path)
            } else {
                path
            };
            let text = if line.is_empty() {
                base
            } else {
                format!("{base}:{line}")
            };
            if let Err(error) = actions::copy_to_clipboard(&text) {
                eprintln!("yoink copy error: {error}");
            }
            return Ok(());
        }
        None => {}
    }

    // Headless mode: `--output <fmt>` prints a one-shot result set and never
    // launches the TUI. `bat` isn't needed here (no live preview), only `rg`.
    if let Some(format) = cli.output {
        ensure_dependency("rg")?;
        let query = cli
            .query_flag
            .clone()
            .or_else(|| cli.query.clone())
            .unwrap_or_default();
        let options = HeadlessOptions {
            query,
            format,
            mode: cli.mode.map(|mode| mode.to_mode()),
            sort: cli.sort.map(|sort| sort.to_sort()),
            case_sensitive: cli.case.map(|case| case.is_sensitive()),
            context: cli.context,
            max_results: cli.max_results,
            blame: cli.blame,
            content_only: cli.content_only,
        };
        run_headless(options, &cwd)?;
        return Ok(());
    }

    ensure_dependency("rg")?;
    ensure_dependency("bat")?;

    let query = cli.query_flag.as_deref().or(cli.query.as_deref());
    tui::run_session(query, &cwd)?;

    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("yoink error: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
        bail_exit();
    }
}

fn bail_exit() {
    std::process::exit(1);
}
