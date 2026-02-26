use crate::actions::{open_in_editor, resolve_target_dir};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run_fzf_session(initial_query: Option<&str>, cwd: &Path, exe_path: &Path) -> Result<()> {
    let exe = exe_path.to_string_lossy();
    let preview = format!("{} __preview {{2}} {{q}} {{3}}", exe);
    let reload = format!("{} __search {{q}}", exe);

    let mut command = Command::new("fzf");
    command
        .arg("--ansi")
        .arg("--delimiter")
        .arg("\t")
        .arg("--with-nth")
        .arg("1")
        .arg("--layout=reverse")
        .arg("--height=100%")
        .arg("--header")
        .arg("Enter: cd to container  |  Ctrl-V: vim  |  Ctrl-O: code  |  Ctrl-S: subl")
        .arg("--preview-window=right:65%:wrap")
        .arg("--preview")
        .arg(preview)
        .arg("--disabled")
        .arg("--print-query")
        .arg("--expect=enter,ctrl-v,ctrl-o,ctrl-s")
        .arg("--bind")
        .arg(format!("start:reload:{reload}"))
        .arg("--bind")
        .arg(format!("change:reload:{reload}"))
        .arg("--prompt")
        .arg("regex> ")
        .current_dir(cwd);

    if let Some(query) = initial_query {
        command.arg("--query").arg(query);
    }

    let output = command
        .output()
        .context("failed to execute fzf for interactive selection")?;

    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();

    let _query_line = lines.next().unwrap_or_default();
    let key = lines.next().unwrap_or("enter");
    let selected_line = lines.next().unwrap_or_default();

    if selected_line.is_empty() {
        return Ok(());
    }

    let (selected_rel_path, _selected_line_num) = parse_selected_line(selected_line);

    if selected_rel_path.is_empty() {
        return Ok(());
    }

    match key {
        "ctrl-v" => {
            if let Err(error) = open_in_editor("vim", cwd, selected_rel_path) {
                eprintln!("yoink editor error: {error}");
            }
            Ok(())
        }
        "ctrl-o" => {
            if let Err(error) = open_in_editor("code", cwd, selected_rel_path) {
                eprintln!("yoink editor error: {error}");
            }
            Ok(())
        }
        "ctrl-s" => {
            if let Err(error) = open_in_editor("subl", cwd, selected_rel_path) {
                eprintln!("yoink editor error: {error}");
            }
            Ok(())
        }
        _ => {
            let target = resolve_target_dir(cwd, selected_rel_path);
            println!("{}", target.display());
            Ok(())
        }
    }
}

fn parse_selected_line(selected_line: &str) -> (&str, Option<usize>) {
    let mut parts = selected_line.splitn(3, '\t');
    let _display = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let line = parts.next().and_then(|raw| {
        if raw.trim().is_empty() {
            None
        } else {
            raw.trim().parse::<usize>().ok()
        }
    });

    (path, line)
}

pub fn run_preview(
    cwd: &Path,
    selected_rel_path: &str,
    query: &str,
    selected_line: Option<usize>,
) -> Result<()> {
    let full = cwd.join(selected_rel_path);
    if full.is_dir() {
        Command::new("ls")
            .arg("-la")
            .arg(&full)
            .status()
            .context("failed to preview directory with ls")?;
        return Ok(());
    }

    let mut bat = Command::new("bat");
    bat.arg("--style=numbers")
        .arg("--color=always");

    if let Some(line_num) = selected_line {
        let context = 30usize;
        let start = if line_num > context { line_num - context } else { 1 };
        let end = line_num + context;
        bat.arg("--highlight-line")
            .arg(line_num.to_string())
            .arg("--line-range")
            .arg(format!("{start}:{end}"));
    } else if !query.trim().is_empty() {
        let rg_output = Command::new("rg")
            .arg("-n")
            .arg("-m")
            .arg("1")
            .arg("--color=never")
            .arg("--no-messages")
            .arg("-e")
            .arg(query)
            .arg(&full)
            .output()
            .context("failed to execute rg for preview line detection")?;

        if rg_output.status.success() {
            let stdout = String::from_utf8_lossy(&rg_output.stdout);
            let first_line = stdout.lines().next().unwrap_or_default();
            let mut parts = first_line.splitn(2, ':');
            if let Some(line_str) = parts.next() {
                if let Ok(line_num) = line_str.parse::<usize>() {
                    let context = 30usize;
                    let start = if line_num > context { line_num - context } else { 1 };
                    let end = line_num + context;
                    bat.arg("--highlight-line")
                        .arg(line_num.to_string())
                        .arg("--line-range")
                        .arg(format!("{start}:{end}"));
                } else {
                    bat.arg("--line-range=:300");
                }
            } else {
                bat.arg("--line-range=:300");
            }
        } else {
            bat.arg("--line-range=:300");
        }
    } else {
        bat.arg("--line-range=:300");
    }

    let status = bat
        .arg(&full)
        .status()
        .context("failed to preview file with bat")?;

    if !status.success() {
        Command::new("sed")
            .arg("-n")
            .arg("1,300p")
            .arg(&full)
            .status()
            .context("failed to preview file with sed fallback")?;
    }

    Ok(())
}

pub fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("failed to resolve current executable path")
}
