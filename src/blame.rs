use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BLAME_SORT_ENV: &str = "YOINK_BLAME_SORT_FILE";

pub fn state_file_path() -> Option<PathBuf> {
    env::var_os(BLAME_SORT_ENV).map(PathBuf::from)
}

pub fn blame_sort_active() -> bool {
    state_file_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

/// Per-session cache directory for blame results. Living next to the state
/// file means it shares the session's PID and is cleaned up when the session
/// ends. Returns None when blame mode is not active (no state file env var).
pub fn blame_cache_dir() -> Option<PathBuf> {
    let state = state_file_path()?;
    let parent = state
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let stem = state
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "yoink-blame".to_string());
    Some(parent.join(format!("{stem}-cache")))
}

pub fn clear_blame_cache() {
    if let Some(dir) = blame_cache_dir() {
        let _ = fs::remove_dir_all(&dir);
    }
}

fn cache_path_for(cache_dir: &Path, abs: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    abs.hash(&mut hasher);
    cache_dir.join(format!("{:016x}.blame", hasher.finish()))
}

/// Cached wrapper around `blame_times_for_file`. The cache is invalidated when
/// the source file's mtime is newer than the cache entry's mtime, so editing a
/// file mid-session will trigger a re-blame on the next read.
pub fn blame_times_cached(cwd: &Path, file: &Path) -> HashMap<usize, i64> {
    let abs = cwd.join(file);

    if let Some(cache_dir) = blame_cache_dir() {
        let cache_file = cache_path_for(&cache_dir, &abs);
        let file_mtime = fs::metadata(&abs).and_then(|m| m.modified()).ok();
        let cache_mtime = fs::metadata(&cache_file).and_then(|m| m.modified()).ok();
        if let (Some(fm), Some(cm)) = (file_mtime, cache_mtime) {
            if cm >= fm {
                if let Ok(content) = fs::read_to_string(&cache_file) {
                    return parse_cache_blob(&content);
                }
            }
        }

        let times = blame_times_for_file(cwd, file);
        let _ = fs::create_dir_all(&cache_dir);
        let mut body = String::new();
        for (line, ts) in &times {
            body.push_str(&format!("{line} {ts}\n"));
        }
        let _ = fs::write(&cache_file, body);
        return times;
    }

    blame_times_for_file(cwd, file)
}

fn parse_cache_blob(content: &str) -> HashMap<usize, i64> {
    let mut map = HashMap::new();
    for raw in content.lines() {
        let mut parts = raw.split_whitespace();
        let Some(line) = parts.next().and_then(|v| v.parse::<usize>().ok()) else {
            continue;
        };
        let Some(ts) = parts.next().and_then(|v| v.parse::<i64>().ok()) else {
            continue;
        };
        map.insert(line, ts);
    }
    map
}

pub fn toggle_blame_sort() -> Result<bool> {
    let Some(path) = state_file_path() else {
        return Ok(false);
    };
    if path.exists() {
        let _ = fs::remove_file(&path);
        Ok(false)
    } else {
        fs::write(&path, b"1")?;
        Ok(true)
    }
}

/// Walk up from `start` looking for a `.git` entry (directory for normal
/// repos, file for submodules/worktrees). Returns the directory that contains
/// `.git`, i.e. the repo's working-tree root. Returns `None` if no `.git` is
/// found before the filesystem root.
///
/// This is used instead of relying on `git rev-parse --show-toplevel` from
/// yoink's cwd because that finds the *enclosing* repo, which can be wrong
/// when yoink is run from a container directory that holds many independent
/// sub-repos (each with its own `.git`).
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current: &Path = start;
    loop {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

/// Returns a map of (1-indexed line number) -> author-time (unix seconds)
/// for the given file. The git command is run from the file's containing
/// repository, so this works even when `cwd` is a parent directory holding
/// multiple independent sub-repos. Returns empty map on any failure.
pub fn blame_times_for_file(cwd: &Path, file: &Path) -> HashMap<usize, i64> {
    let mut out = HashMap::new();
    let abs = cwd.join(file);
    let Some(repo_root) = abs.parent().and_then(find_repo_root) else {
        return out;
    };
    let rel_to_repo = match abs.strip_prefix(&repo_root) {
        Ok(p) => p.to_path_buf(),
        Err(_) => return out,
    };
    let output = match Command::new("git")
        .arg("blame")
        .arg("--line-porcelain")
        .arg("--")
        .arg(&rel_to_repo)
        .current_dir(&repo_root)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return out,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_line: Option<usize> = None;
    let mut current_time: Option<i64> = None;

    for raw in stdout.lines() {
        if let Some(first) = raw.chars().next() {
            // Header lines start with sha (40 hex). Content lines start with a tab.
            if first == '\t' {
                if let (Some(line), Some(ts)) = (current_line, current_time) {
                    out.insert(line, ts);
                }
                current_line = None;
                current_time = None;
                continue;
            }
        }

        let mut parts = raw.splitn(4, ' ');
        let key = parts.next().unwrap_or("");
        match key {
            "author-time" => {
                if let Some(val) = parts.next() {
                    current_time = val.parse::<i64>().ok();
                }
            }
            _ => {
                // header line: <sha> <orig-line> <final-line> [num-lines]
                if key.len() == 40 && key.chars().all(|c| c.is_ascii_hexdigit()) {
                    // skip orig-line
                    parts.next();
                    if let Some(final_line) = parts.next() {
                        current_line = final_line.parse::<usize>().ok();
                    }
                }
            }
        }
    }

    out
}

/// Get the most-recent commit author-time for one file. Returns the raw git
/// stderr on failure so callers can show *why* blame information is
/// unavailable instead of silently hiding the cause. Runs git inside the
/// file's actual containing repo (handles nested independent repos).
pub fn file_last_touched_verbose(cwd: &Path, file: &Path) -> Result<i64, String> {
    let abs = cwd.join(file);
    let repo_root = abs
        .parent()
        .and_then(find_repo_root)
        .ok_or_else(|| format!("no .git ancestor found for {}", abs.display()))?;
    let rel_to_repo = abs
        .strip_prefix(&repo_root)
        .map_err(|_| "file is not under its detected repo root".to_string())?;

    let output = Command::new("git")
        .arg("log")
        .arg("-1")
        .arg("--format=%at")
        .arg("--")
        .arg(rel_to_repo)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if msg.is_empty() {
            format!("git log exited with status {}", output.status)
        } else {
            msg
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "no commits touch this file in {} (untracked or never committed)",
            repo_root.display()
        ));
    }
    trimmed
        .parse::<i64>()
        .map_err(|e| format!("could not parse git timestamp '{trimmed}': {e}"))
}

/// Quick check: is `cwd` inside a git working tree? Returns the toplevel path
/// on success, or git's stderr on failure.
pub fn git_toplevel(cwd: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if msg.is_empty() {
            format!("git rev-parse exited with status {}", output.status)
        } else {
            msg
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Returns a short blame summary for a specific line: "abc1234 2024-05-11 Author Name"
pub fn blame_line_summary(cwd: &Path, file: &Path, line: usize) -> Option<String> {
    let abs = cwd.join(file);
    let repo_root = abs.parent().and_then(find_repo_root)?;
    let rel_to_repo = abs.strip_prefix(&repo_root).ok()?;
    let range = format!("{line},{line}");
    let output = Command::new("git")
        .arg("blame")
        .arg("-L")
        .arg(&range)
        .arg("--date=short")
        .arg("-w")
        .arg("--")
        .arg(rel_to_repo)
        .current_dir(&repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?;
    // Format: <sha> (<author> <date> <time> <tz> <line>) <content>
    let sha_end = first.find(' ')?;
    let sha = &first[..sha_end.min(8)];
    let paren_start = first.find('(')?;
    let paren_end = first.find(')')?;
    let inside = &first[paren_start + 1..paren_end];
    // With --date=short the inside tokens are: <author...> <YYYY-MM-DD> <lineno>
    let tokens: Vec<&str> = inside.split_whitespace().collect();
    if tokens.len() < 3 {
        return Some(format!("{sha} {inside}"));
    }
    let author = tokens[..tokens.len() - 2].join(" ");
    let date = tokens[tokens.len() - 2];
    Some(format!("{sha} {date} {author}"))
}

pub fn format_unix_date(ts: i64) -> String {
    // Minimal yyyy-mm-dd formatter without chrono dependency.
    // Algorithm from Howard Hinnant's date library.
    let days = ts.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}
