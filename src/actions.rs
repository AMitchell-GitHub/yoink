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
