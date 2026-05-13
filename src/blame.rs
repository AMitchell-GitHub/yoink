use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BLAME_SORT_ENV: &str = "YOINK_BLAME_SORT_FILE";
/// Per-fzf-session cache directory env var. Set unconditionally by
/// `ui::run_fzf_session` so blame results can be cached regardless of whether
/// the user has toggled blame-sort mode — that way previewing many files in
/// regular search mode only pays the git-blame cost once per file.
pub const SESSION_CACHE_ENV: &str = "YOINK_CACHE_DIR";

pub fn state_file_path() -> Option<PathBuf> {
    env::var_os(BLAME_SORT_ENV).map(PathBuf::from)
}

pub fn blame_sort_active() -> bool {
    state_file_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

pub fn session_cache_dir() -> Option<PathBuf> {
    env::var_os(SESSION_CACHE_ENV).map(PathBuf::from)
}

pub fn clear_session_cache() {
    if let Some(dir) = session_cache_dir() {
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Back-compat name used by `__blame_collect` when toggling blame-sort ON, to
/// force fresh blame data (mtime-based invalidation might miss mid-session
/// commits where the working file is unchanged).
pub fn clear_blame_cache() {
    clear_session_cache();
}

fn cache_path_for(cache_dir: &Path, abs: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    abs.hash(&mut hasher);
    cache_dir.join(format!("{:016x}.blame", hasher.finish()))
}

/// Rich per-line blame data — what `--line-porcelain` gives us. Used by both
/// the blame-sort path (which only needs timestamps) and the preview header
/// (which also wants the sha and author name).
#[derive(Debug, Clone)]
pub struct LineBlame {
    pub timestamp: i64,
    pub sha: String,
    pub author: String,
}

/// Per-line blame for the focused line only. Uses `git blame -L N,N` which
/// stops walking history once it finds the commit that introduced that one
/// line — typically 50–500ms even on huge files in deep repos, versus 1.5s+
/// for a whole-file porcelain blame. Results are merged into the same
/// per-file cache so callers like preview-on-arrow-key get instant repeat
/// lookups, and blame-sort can opportunistically use whatever lines are
/// already cached.
pub fn blame_for_line_cached(cwd: &Path, file: &Path, line: usize) -> Option<LineBlame> {
    let abs = cwd.join(file);

    // Cache hit: existing whole-file or per-line entry.
    if let Some(cache_dir) = session_cache_dir() {
        let cache_file = cache_path_for(&cache_dir, &abs);
        if let Some(map) = read_cache_if_fresh(&cache_file, &abs) {
            if let Some(info) = map.get(&line) {
                return Some(info.clone());
            }
        }
    }

    // Cache miss: run a single-line blame.
    let info = blame_one_line(cwd, file, line)?;

    // Merge into the on-disk cache so subsequent previews hit it. Read the
    // current map first so we don't clobber other lines that previous calls
    // populated.
    if let Some(cache_dir) = session_cache_dir() {
        let cache_file = cache_path_for(&cache_dir, &abs);
        let mut map = read_cache_if_fresh(&cache_file, &abs).unwrap_or_default();
        map.insert(line, info.clone());
        let _ = fs::create_dir_all(&cache_dir);
        let mut body = String::new();
        for (l, b) in &map {
            body.push_str(&format!("{}\t{}\t{}\t{}\n", l, b.timestamp, b.sha, b.author));
        }
        let _ = fs::write(&cache_file, body);
    }

    Some(info)
}

fn read_cache_if_fresh(cache_file: &Path, abs: &Path) -> Option<HashMap<usize, LineBlame>> {
    let file_mtime = fs::metadata(abs).and_then(|m| m.modified()).ok()?;
    let cache_mtime = fs::metadata(cache_file).and_then(|m| m.modified()).ok()?;
    if cache_mtime < file_mtime {
        return None;
    }
    let content = fs::read_to_string(cache_file).ok()?;
    Some(parse_cache_blob(&content))
}

/// Fast file-level summary: most recent commit's author-time + author name.
/// Used by the preview header when the focused row is a file entry rather
/// than a match line. `git log -1` is ~30ms even on big repos.
pub fn file_last_touched(cwd: &Path, file: &Path) -> Option<(i64, String)> {
    let abs = cwd.join(file);
    let repo_root = abs.parent().and_then(find_repo_root)?;
    let rel_to_repo = abs.strip_prefix(&repo_root).ok()?;
    let output = Command::new("git")
        .arg("log")
        .arg("-1")
        .arg("--format=%at%x09%an")
        .arg("--")
        .arg(rel_to_repo)
        .current_dir(&repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?;
    let mut parts = line.splitn(2, '\t');
    let ts: i64 = parts.next()?.parse().ok()?;
    let author = parts.next().unwrap_or("").to_string();
    Some((ts, author))
}

fn blame_one_line(cwd: &Path, file: &Path, line: usize) -> Option<LineBlame> {
    let abs = cwd.join(file);
    let repo_root = abs.parent().and_then(find_repo_root)?;
    let rel_to_repo = abs.strip_prefix(&repo_root).ok()?;

    let range = format!("{line},{line}");
    let output = Command::new("git")
        .arg("blame")
        .arg("-L")
        .arg(&range)
        .arg("--line-porcelain")
        .arg("--")
        .arg(rel_to_repo)
        .current_dir(&repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut sha = String::new();
    let mut author = String::new();
    let mut timestamp: Option<i64> = None;

    for raw in stdout.lines() {
        if raw.starts_with('\t') {
            break;
        }
        if let Some(rest) = raw.strip_prefix("author ") {
            author = rest.replace('\t', " ");
            continue;
        }
        if let Some(rest) = raw.strip_prefix("author-time ") {
            timestamp = rest.split_whitespace().next().and_then(|v| v.parse().ok());
            continue;
        }
        if sha.is_empty()
            && raw.len() >= 40
            && raw[..40].chars().all(|c| c.is_ascii_hexdigit())
        {
            sha = raw[..40].to_string();
        }
    }

    let timestamp = timestamp?;
    if sha.is_empty() {
        return None;
    }
    Some(LineBlame {
        sha,
        timestamp,
        author,
    })
}

/// Non-blocking cache probe: returns `Some(map)` only if a fresh cache entry
/// already exists on disk for this file. Never spawns git. Used by callers
/// that want to decide *whether* to wait for blame data (e.g. the preview
/// pane chooses blame-at-top on a warm cache, blame-at-bottom-after-content
/// on a cold cache so the file body appears immediately).
pub fn try_blame_from_cache(cwd: &Path, file: &Path) -> Option<HashMap<usize, LineBlame>> {
    let cache_dir = session_cache_dir()?;
    let abs = cwd.join(file);
    let cache_file = cache_path_for(&cache_dir, &abs);
    let file_mtime = fs::metadata(&abs).and_then(|m| m.modified()).ok()?;
    let cache_mtime = fs::metadata(&cache_file).and_then(|m| m.modified()).ok()?;
    if cache_mtime < file_mtime {
        return None;
    }
    let content = fs::read_to_string(&cache_file).ok()?;
    Some(parse_cache_blob(&content))
}

/// Convenience accessors that operate on an already-fetched blame map. Used
/// by the preview pane to format the same blame info regardless of whether
/// the data came from a cache hit (header position) or a deferred fetch
/// (footer position after bat).
pub fn line_summary_from_map(map: &HashMap<usize, LineBlame>, line: usize) -> Option<String> {
    let info = map.get(&line)?;
    let sha_short: String = info.sha.chars().take(8).collect();
    Some(format!(
        "{sha_short} {} {}",
        format_unix_date(info.timestamp),
        info.author
    ))
}

pub fn latest_change_from_map(map: &HashMap<usize, LineBlame>) -> Option<(i64, String)> {
    map.values()
        .max_by_key(|b| b.timestamp)
        .map(|b| (b.timestamp, b.author.clone()))
}

/// Cached version of `blame_for_file`. Cache entries are invalidated when the
/// source file's mtime is newer than the cache entry's mtime.
pub fn blame_for_file_cached(cwd: &Path, file: &Path) -> HashMap<usize, LineBlame> {
    let abs = cwd.join(file);

    if let Some(cache_dir) = session_cache_dir() {
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

        let blame = blame_for_file(cwd, file);
        let _ = fs::create_dir_all(&cache_dir);
        let mut body = String::new();
        for (line, info) in &blame {
            body.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                line, info.timestamp, info.sha, info.author
            ));
        }
        let _ = fs::write(&cache_file, body);
        return blame;
    }

    blame_for_file(cwd, file)
}

/// Convenience wrapper for callers that only need timestamps (search.rs sort).
pub fn blame_times_cached(cwd: &Path, file: &Path) -> HashMap<usize, i64> {
    blame_for_file_cached(cwd, file)
        .into_iter()
        .map(|(line, info)| (line, info.timestamp))
        .collect()
}

fn parse_cache_blob(content: &str) -> HashMap<usize, LineBlame> {
    let mut map = HashMap::new();
    for raw in content.lines() {
        let mut parts = raw.splitn(4, '\t');
        let Some(line) = parts.next().and_then(|v| v.parse::<usize>().ok()) else {
            continue;
        };
        let Some(ts) = parts.next().and_then(|v| v.parse::<i64>().ok()) else {
            continue;
        };
        let sha = parts.next().unwrap_or("").to_string();
        let author = parts.next().unwrap_or("").to_string();
        map.insert(
            line,
            LineBlame {
                timestamp: ts,
                sha,
                author,
            },
        );
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

/// Returns a map of (1-indexed line number) -> rich blame info for the given
/// file. The git command is run from the file's containing repository, so
/// this works even when `cwd` is a parent directory holding multiple
/// independent sub-repos. Returns empty map on any failure.
pub fn blame_for_file(cwd: &Path, file: &Path) -> HashMap<usize, LineBlame> {
    let mut out: HashMap<usize, LineBlame> = HashMap::new();
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
    let mut current_sha = String::new();
    let mut current_author = String::new();

    for raw in stdout.lines() {
        if let Some(first) = raw.chars().next() {
            // Header lines start with sha (40 hex). Content lines start with a tab.
            if first == '\t' {
                if let (Some(line), Some(ts)) = (current_line, current_time) {
                    out.insert(
                        line,
                        LineBlame {
                            timestamp: ts,
                            sha: current_sha.clone(),
                            author: current_author.clone(),
                        },
                    );
                }
                current_line = None;
                current_time = None;
                continue;
            }
        }

        // `author <name>` may contain spaces — split into 2 instead of 4 so
        // the whole rest-of-line becomes the author name. Other keys use a
        // larger split.
        if let Some(author) = raw.strip_prefix("author ") {
            // Strip any tabs (cache format uses tabs as field separators).
            current_author = author.replace('\t', " ");
            continue;
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
                    current_sha = key.to_string();
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
