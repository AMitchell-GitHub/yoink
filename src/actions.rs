use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use which::which;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // Result actions — operate on the highlighted entry.
    Cd,
    Vim,
    Vi,
    Nano,
    Cat,
    VsCode,
    Sublime,
    Explorer,
    CopyPath,
    CopyName,
    // Query-editing actions — operate on the query box, no selection needed.
    ClearQuery,
    DeleteWord,
    LineStart,
    LineEnd,
}

impl Action {
    pub fn from_token(s: &str) -> Option<Action> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cd" => Some(Action::Cd),
            "vim" => Some(Action::Vim),
            "vi" => Some(Action::Vi),
            "nano" => Some(Action::Nano),
            "cat" => Some(Action::Cat),
            "vscode" | "code" => Some(Action::VsCode),
            "sublime" | "subl" => Some(Action::Sublime),
            "explorer" | "open" | "xdg-open" => Some(Action::Explorer),
            "copy_path" | "copy-path" | "copypath" => Some(Action::CopyPath),
            "copy_name" | "copy-name" | "copyname" => Some(Action::CopyName),
            "clear_query" | "clear-query" | "clearquery" => Some(Action::ClearQuery),
            "delete_word" | "delete-word" | "deleteword" => Some(Action::DeleteWord),
            "line_start" | "line-start" | "linestart" => Some(Action::LineStart),
            "line_end" | "line-end" | "lineend" => Some(Action::LineEnd),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn token(self) -> &'static str {
        match self {
            Action::Cd => "cd",
            Action::Vim => "vim",
            Action::Vi => "vi",
            Action::Nano => "nano",
            Action::Cat => "cat",
            Action::VsCode => "vscode",
            Action::Sublime => "sublime",
            Action::Explorer => "explorer",
            Action::CopyPath => "copy_path",
            Action::CopyName => "copy_name",
            Action::ClearQuery => "clear_query",
            Action::DeleteWord => "delete_word",
            Action::LineStart => "line_start",
            Action::LineEnd => "line_end",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Cd => "cd",
            Action::Vim => "vim",
            Action::Vi => "vi",
            Action::Nano => "nano",
            Action::Cat => "cat",
            Action::VsCode => "code",
            Action::Sublime => "subl",
            Action::Explorer => "open",
            Action::CopyPath => "copy path",
            Action::CopyName => "copy name",
            Action::ClearQuery => "clear query",
            Action::DeleteWord => "delete word",
            Action::LineStart => "jump to start of query",
            Action::LineEnd => "jump to end of query",
        }
    }
}

pub fn resolve_target_dir(cwd: &Path, selected_rel_path: &str) -> PathBuf {
    let selected = cwd.join(selected_rel_path);
    match selected.parent() {
        Some(parent) => parent.to_path_buf(),
        None => cwd.to_path_buf(),
    }
}

/// "cd into the folder if the highlighted result is a directory, else its
/// parent." Powers the configurable `cd` open-action.
pub fn cd_target(cwd: &Path, selected_rel_path: &str) -> PathBuf {
    let full = cwd.join(selected_rel_path);
    if full.is_dir() {
        full
    } else {
        resolve_target_dir(cwd, selected_rel_path)
    }
}

/// Run a terminal app that takes over the tty (vim/vi/nano/cat). Blocks
/// until the user quits it; the TUI is suspended by the caller for the
/// duration.
///
/// **Critical**: yoink's own stdout is captured by the shell wrapper
/// `target="$(yoink)"` (a pipe) for the cd-via-stdout contract. If we let
/// the editor inherit that stdout it sees a pipe, not a terminal, and
/// refuses to run with "Output is not to a terminal". We open `/dev/tty`
/// directly and hand the child its own tty triplet for stdin/stdout/stderr.
pub fn open_in_editor(editor_cmd: &str, cwd: &Path, selected_rel_path: &str) -> Result<()> {
    which(editor_cmd)
        .with_context(|| format!("editor command not found in PATH: {editor_cmd}"))?;

    let full = cwd.join(selected_rel_path);

    let tty_in = std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .context("failed to open /dev/tty for editor stdin")?;
    let tty_out = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .context("failed to open /dev/tty for editor stdout")?;
    let tty_err = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .context("failed to open /dev/tty for editor stderr")?;

    let status = Command::new(editor_cmd)
        .arg(full)
        .stdin(Stdio::from(tty_in))
        .stdout(Stdio::from(tty_out))
        .stderr(Stdio::from(tty_err))
        .status()
        .with_context(|| format!("failed to launch editor command: {editor_cmd}"))?;

    if !status.success() {
        anyhow::bail!("editor command exited unsuccessfully: {editor_cmd}");
    }

    Ok(())
}

/// Spawn a GUI app or file-manager (code/subl/xdg-open) detached — don't block
/// the result list, and don't let its stdout/stderr leak into fzf. Used by
/// the `execute-silent` fzf wrapper.
pub fn open_detached(cmd: &str, cwd: &Path, selected_rel_path: &str) -> Result<()> {
    which(cmd).with_context(|| format!("command not found in PATH: {cmd}"))?;

    let full = cwd.join(selected_rel_path);
    Command::new(cmd)
        .arg(full)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch command: {cmd}"))?;

    Ok(())
}

pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let (cmd, args) = detect_clipboard()?;
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn clipboard command: {cmd}"))?;
    use std::io::Write;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .context("failed to write to clipboard command stdin")?;
    }
    let status = child.wait().context("clipboard command did not complete")?;
    if !status.success() {
        anyhow::bail!("clipboard command exited with status {status}");
    }
    Ok(())
}

fn detect_clipboard() -> Result<(&'static str, &'static [&'static str])> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && which("wl-copy").is_ok() {
        return Ok(("wl-copy", &[]));
    }
    if which("xclip").is_ok() {
        return Ok(("xclip", &["-selection", "clipboard"]));
    }
    if which("xsel").is_ok() {
        return Ok(("xsel", &["--clipboard", "--input"]));
    }
    anyhow::bail!(
        "no clipboard tool found; install xclip or xsel (X11) or wl-clipboard (Wayland)"
    )
}
