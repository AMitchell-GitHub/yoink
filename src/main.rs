mod actions;
mod blame;
mod cli;
mod keys;
mod search;
mod settings;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, InternalCommand};
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

    ensure_dependency("rg")?;
    ensure_dependency("bat")?;

    tui::run_session(cli.query.as_deref(), &cwd)?;

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
