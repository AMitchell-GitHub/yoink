use crate::actions::{open_in_editor, resolve_target_dir};
use crate::blame::BLAME_SORT_ENV;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run_fzf_session(initial_query: Option<&str>, cwd: &Path, exe_path: &Path) -> Result<()> {
    let exe = exe_path.to_string_lossy();
    let preview = format!("{} __preview {{2}} {{q}} {{3}}", exe);
    let reload = format!("{} __search {{q}}", exe);
    let toggle = format!("{} __toggle_blame", exe);
    let blame_collect = format!("{} __blame_collect {{q}}", exe);
    let prompt_cmd = format!("{} __prompt", exe);

    // Per-session state file used to flag blame-sort mode. Ensure it does not
    // exist at startup so we begin in regex mode.
    let state_file = std::env::temp_dir().join(format!("yoink-blame-{}", std::process::id()));
    let _ = std::fs::remove_file(&state_file);

    let mut command = Command::new("fzf");
    command
        .env(BLAME_SORT_ENV, &state_file)
        .arg("--ansi")
        .arg("--delimiter")
        .arg("\t")
        .arg("--with-nth")
        .arg("1")
        .arg("--layout=reverse")
        .arg("--height=100%")
        .arg("--header")
        .arg("Enter: cd  |  Ctrl-V: vim  |  Ctrl-O: code  |  Ctrl-S: subl  |  Ctrl-B: blame-sort")
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
        // Ctrl-B sequence:
        //   1. flip the blame-mode state file (silent),
        //   2. hand the terminal to `__blame_collect` so it can draw an
        //      in-place progress bar while it pre-populates the blame cache
        //      (this is a no-op exit when the toggle just turned blame OFF),
        //   3. reload the list — `__search` now reads from the warm cache,
        //   4. update the prompt label.
        .arg("--bind")
        .arg(format!(
            "ctrl-b:execute-silent({toggle})+execute({blame_collect})+reload({reload})+transform-prompt({prompt_cmd})"
        ))
        .arg("--prompt")
        .arg("regex> ")
        .current_dir(cwd);

    if let Some(query) = initial_query {
        command.arg("--query").arg(query);
    }

    let output = command
        .output()
        .context("failed to execute fzf for interactive selection")?;

    let _ = std::fs::remove_file(&state_file);
    crate::blame::clear_blame_cache();

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

    print_blame_header(cwd, Path::new(selected_rel_path), selected_line);

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

fn print_blame_header(cwd: &Path, rel: &Path, line: Option<usize>) {
    use crate::blame::{
        blame_line_summary, file_last_touched_verbose, format_unix_date, git_toplevel,
    };

    print!("\x1b[1;33m📜 git blame\x1b[0m  ");

    // First make sure we're actually inside a git repo. If not, surface the
    // real reason so the user can see why blame is unavailable.
    match git_toplevel(cwd) {
        Err(err) => {
            println!("\x1b[1;31mgit unavailable here\x1b[0m");
            println!("\x1b[2;37mcwd:\x1b[0m {}", cwd.display());
            println!("\x1b[2;37mgit:\x1b[0m {err}");
            println!("\x1b[2;37m────────────────────────────────────────\x1b[0m");
            return;
        }
        Ok(top) => {
            if let Some(line) = line {
                if let Some(summary) = blame_line_summary(cwd, rel, line) {
                    println!("\x1b[1;36mL{line}\x1b[0m  \x1b[37m{summary}\x1b[0m");
                    println!("\x1b[2;37m────────────────────────────────────────\x1b[0m");
                    return;
                }
            }
            match file_last_touched_verbose(cwd, rel) {
                Ok(ts) => {
                    println!(
                        "\x1b[37mlast touched\x1b[0m \x1b[1;35m{}\x1b[0m",
                        format_unix_date(ts)
                    );
                }
                Err(err) => {
                    println!("\x1b[1;31mblame unavailable\x1b[0m");
                    println!("\x1b[2;37mrepo:\x1b[0m {top}");
                    println!("\x1b[2;37mfile:\x1b[0m {}", rel.display());
                    println!("\x1b[2;37mgit:\x1b[0m  {err}");
                }
            }
        }
    }
    println!("\x1b[2;37m────────────────────────────────────────\x1b[0m");
}

pub fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("failed to resolve current executable path")
}
