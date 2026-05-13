mod actions;
mod blame;
mod cli;
mod search;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, InternalCommand};
use search::{run_blame_collect, run_search_streaming};
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
        Some(InternalCommand::Preview { path, query, line }) => {
            ensure_dependency("bat")?;
            ensure_dependency("rg")?;
            return ui::run_preview(&cwd, &path, &query, line);
        }
        Some(InternalCommand::ToggleBlame) => {
            let _ = blame::toggle_blame_sort();
            return Ok(());
        }
        Some(InternalCommand::Prompt) => {
            if blame::blame_sort_active() {
                print!("blame> ");
            } else {
                print!("regex> ");
            }
            return Ok(());
        }
        Some(InternalCommand::BlameCollect { query }) => {
            ensure_dependency("rg")?;
            return run_blame_collect(&query, &cwd);
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

    ensure_dependency("fzf")?;
    ensure_dependency("rg")?;
    ensure_dependency("bat")?;

    let exe = ui::current_exe()?;
    ui::run_fzf_session(cli.query.as_deref(), &cwd, &exe)?;

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
