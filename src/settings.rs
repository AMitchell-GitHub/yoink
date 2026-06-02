//! Config-file write helpers. The interactive settings UI lives in the
//! TUI overlay (`src/tui.rs`); this module just provides the read-modify-write
//! primitive used to persist toggles. The verbose default that `tui::run_session`
//! materializes on first launch is the shipped `.yoink-config` reference,
//! embedded via `tui::DEFAULT_CONFIG_REFERENCE`.

use crate::search::config_path;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Read-modify-write a single `key=value` scalar setting in the active
/// config file. Preserves existing comments, blank lines, bind lines, and
/// ignore-glob lines verbatim. If the key isn't already present the line is
/// appended; if the config file doesn't exist it's created with a sensible
/// default first.
pub fn write_setting(key: &str, value: &str) -> Result<()> {
    let path = config_path().context("no $HOME — can't write to a yoink config file")?;
    write_setting_to(&path, key, value)
}

fn write_setting_to(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut existing = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        // The TUI's `ensure_config_exists` writes the full annotated default
        // before any toggle is possible, so this branch is rare. Fall back
        // to an empty file so the new key is appended cleanly.
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        String::new()
    };

    let trailing_newline = existing.ends_with('\n');
    if !trailing_newline {
        existing.push('\n');
    }

    let key_lower = key.to_ascii_lowercase();
    let mut replaced = false;
    let mut out_lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            out_lines.push(line.to_string());
            continue;
        }
        if let Some((lkey, _)) = trimmed.split_once('=') {
            if lkey.trim().to_ascii_lowercase() == key_lower {
                out_lines.push(format!("{key} = {value}"));
                replaced = true;
                continue;
            }
        }
        out_lines.push(line.to_string());
    }
    if !replaced {
        // Insert near the top, before the ignore-glob lines.
        let mut insert_at = out_lines.len();
        for (idx, line) in out_lines.iter().enumerate() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if !t.contains('=') {
                insert_at = idx;
                break;
            }
        }
        out_lines.insert(insert_at, format!("{key} = {value}"));
    }

    let mut body = out_lines.join("\n");
    body.push('\n');

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body.as_bytes())
        .with_context(|| format!("failed to write temp config at {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename temp config into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn write_setting_replaces_existing_scalar() {
        let f = NamedTempFile::new().unwrap();
        fs::write(
            f.path(),
            "# comment\n\
include_hidden=false\n\
search_mode=regex\n\
case_sensitive=true\n\
\n\
.git/**\n\
node_modules/**\n",
        )
        .unwrap();

        write_setting_to(f.path(), "search_mode", "glob").unwrap();
        let body = fs::read_to_string(f.path()).unwrap();
        assert!(body.contains("search_mode = glob"));
        assert!(!body.contains("search_mode=regex"));
        assert!(!body.contains("search_mode = regex"));
        assert!(body.contains(".git/**"));
        assert!(body.contains("# comment"));
    }

    #[test]
    fn write_setting_inserts_when_missing() {
        let f = NamedTempFile::new().unwrap();
        fs::write(
            f.path(),
            "include_hidden=false\n\
\n\
.git/**\n",
        )
        .unwrap();
        write_setting_to(f.path(), "search_mode", "glob").unwrap();
        let body = fs::read_to_string(f.path()).unwrap();
        assert!(body.contains("search_mode = glob"));
        assert!(body.contains(".git/**"));
        assert!(
            body.find("search_mode = glob").unwrap() < body.find(".git/**").unwrap(),
            "setting should be inserted before glob lines:\n{body}"
        );
    }

    #[test]
    fn write_setting_creates_file_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.cfg");
        write_setting_to(&path, "search_mode", "regex").unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("search_mode = regex"));
    }
}
