use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use which::which;

pub fn resolve_target_dir(cwd: &Path, selected_rel_path: &str) -> PathBuf {
    let selected = cwd.join(selected_rel_path);
    match selected.parent() {
        Some(parent) => parent.to_path_buf(),
        None => cwd.to_path_buf(),
    }
}

pub fn open_in_editor(editor_cmd: &str, cwd: &Path, selected_rel_path: &str) -> Result<()> {
    which(editor_cmd)
        .with_context(|| format!("editor command not found in PATH: {editor_cmd}"))?;

    let full = cwd.join(selected_rel_path);
    let status = Command::new(editor_cmd)
        .arg(full)
        .status()
        .with_context(|| format!("failed to launch editor command: {editor_cmd}"))?;

    if !status.success() {
        anyhow::bail!("editor command exited unsuccessfully: {editor_cmd}");
    }

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
