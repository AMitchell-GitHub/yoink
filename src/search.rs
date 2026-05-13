use crate::blame::{
    blame_sort_active, blame_times_cached, find_repo_root, format_unix_date,
};
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[cfg(target_family = "unix")]
use std::os::unix::fs::MetadataExt;

// Patterns are matched against the relative path of each walk entry. To prune
// directories at *any* depth (not just at the cwd root), the pattern must match
// the directory's path itself — `**/name` — so `WalkDir::filter_entry` can
// return `false` and skip descending. The previous `name/**` form only matched
// children, which means we used to walk into every nested `node_modules` and
// `.git` before filtering individual files. In a tree with many sub-repos this
// added seconds per search.
const DEFAULT_IGNORE_GLOBS: &[&str] = &["**/.git", "**/node_modules"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Depth,
    Alphabetical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub is_dir: bool,
    pub path_match: bool,
    pub content_match: bool,
}

#[derive(Debug, Clone)]
struct YoinkSettings {
    include_hidden: bool,
    include_mounts: bool,
    include_symlinks: bool,
    sort_mode: SortMode,
    globset: GlobSet,
    globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub display: String,
    pub path: PathBuf,
    pub line: Option<usize>,
}

#[derive(Debug, Clone)]
struct Occurrence {
    line: usize,
    column: usize,
    snippet: String,
}

fn is_hidden_path(rel: &Path) -> bool {
    rel.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name.starts_with('.')
    })
}

fn parse_bool_setting(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_sort_mode_setting(value: &str) -> Option<SortMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "depth" => Some(SortMode::Depth),
        "alphabetical" => Some(SortMode::Alphabetical),
        _ => None,
    }
}

fn yoinkignore_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("YOINKIGNORE_PATH") {
        return Some(PathBuf::from(path));
    }

    env::var_os("HOME").map(|home| PathBuf::from(home).join(".yoinkignore"))
}

fn load_settings() -> Result<YoinkSettings> {
    let mut include_hidden = false;
    let mut include_mounts = false;
    let mut include_symlinks = false;
    let mut sort_mode = SortMode::Depth;
    let mut globs: Vec<String> = DEFAULT_IGNORE_GLOBS
        .iter()
        .map(|pattern| pattern.to_string())
        .collect();

    if let Some(ignore_file) = yoinkignore_path() {
        if ignore_file.exists() {
            let content = fs::read_to_string(&ignore_file)
                .with_context(|| format!("failed to read {}", ignore_file.display()))?;

            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                if let Some((raw_key, raw_value)) = trimmed.split_once('=') {
                    let key = raw_key.trim().to_ascii_lowercase();
                    let value = raw_value.trim();
                    match key.as_str() {
                        "include_hidden" => {
                            include_hidden = parse_bool_setting(value).with_context(|| {
                                format!("invalid include_hidden value in {}: {value}", ignore_file.display())
                            })?;
                            continue;
                        }
                        "include_mounts" => {
                            include_mounts = parse_bool_setting(value).with_context(|| {
                                format!("invalid include_mounts value in {}: {value}", ignore_file.display())
                            })?;
                            continue;
                        }
                        "include_symlinks" => {
                            include_symlinks = parse_bool_setting(value).with_context(|| {
                                format!("invalid include_symlinks value in {}: {value}", ignore_file.display())
                            })?;
                            continue;
                        }
                        "sort_mode" => {
                            sort_mode = parse_sort_mode_setting(value).with_context(|| {
                                format!("invalid sort_mode value in {}: {value}", ignore_file.display())
                            })?;
                            continue;
                        }
                        _ => {}
                    }
                }

                globs.push(trimmed.to_string());
            }
        }
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in &globs {
        builder.add(
            Glob::new(pattern)
                .with_context(|| format!("invalid ~/.yoinkignore glob: {pattern}"))?,
        );
    }

    let globset = builder.build().context("failed building ignore glob set")?;
    Ok(YoinkSettings {
        include_hidden,
        include_mounts,
        include_symlinks,
        sort_mode,
        globset,
        globs,
    })
}

/// Walk the directory tree and return path-name matches as `Candidate`s.
/// Does NOT run rg — only directory traversal with regex filtering on
/// path components. This is the fast half of `build_search_entries`, and is
/// also used as the first step of `build_candidates`.
fn walk_path_candidates(
    query: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<HashMap<PathBuf, Candidate>> {
    let mut map: HashMap<PathBuf, Candidate> = HashMap::new();

    #[cfg(target_family = "unix")]
    let root_dev = if settings.include_mounts {
        None
    } else {
        Some(
            fs::metadata(cwd)
                .with_context(|| format!("failed to stat search root: {}", cwd.display()))?
                .dev(),
        )
    };

    let regex = if query.is_empty() {
        None
    } else {
        Some(Regex::new(query).with_context(|| format!("invalid regex query: {query}"))?)
    };

    let iter = WalkDir::new(cwd)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let path = entry.path();
            if path == cwd {
                return true;
            }

            if !settings.include_symlinks && entry.path_is_symlink() {
                return false;
            }

            let rel = match path.strip_prefix(cwd) {
                Ok(v) => v,
                Err(_) => return false,
            };

            if (!settings.include_hidden && is_hidden_path(rel)) || settings.globset.is_match(rel) {
                return false;
            }

            #[cfg(target_family = "unix")]
            {
                if let Some(root_dev) = root_dev {
                    if entry.file_type().is_dir() {
                        if let Ok(metadata) = fs::metadata(path) {
                            if metadata.dev() != root_dev {
                                return false;
                            }
                        }
                    }
                }
            }

            true
        });

    for entry in iter.filter_map(Result::ok) {
        let path = entry.path();
        if path == cwd {
            continue;
        }

        let rel = match path.strip_prefix(cwd) {
            Ok(v) => v.to_path_buf(),
            Err(_) => continue,
        };

        let path_str = rel.to_string_lossy();
        let file_name = rel
            .file_name()
            .map(|v| v.to_string_lossy())
            .unwrap_or_else(|| path_str.clone());

        let is_match = match &regex {
            None => true,
            Some(re) => re.is_match(&path_str) || re.is_match(&file_name),
        };

        if is_match {
            map.entry(rel.clone())
                .and_modify(|candidate| candidate.path_match = true)
                .or_insert(Candidate {
                    path: rel,
                    is_dir: entry.file_type().is_dir(),
                    path_match: true,
                    content_match: false,
                });
        }
    }

    Ok(map)
}

// Kept as a public API for the integration tests in tests/search.rs, which
// assert combined path + content matching against small temp fixtures. The
// optimized hot path used by the running binary lives in
// `build_search_entries` and bypasses this function.
#[allow(dead_code)]
pub fn build_candidates(query: &str, cwd: &Path) -> Result<Vec<Candidate>> {
    let settings = load_settings()?;
    let mut map = walk_path_candidates(query, cwd, &settings)?;

    #[cfg(target_family = "unix")]
    let root_dev = if settings.include_mounts {
        None
    } else {
        Some(
            fs::metadata(cwd)
                .with_context(|| format!("failed to stat search root: {}", cwd.display()))?
                .dev(),
        )
    };

    if !query.is_empty() {
        let mut rg_command = Command::new("rg");
        rg_command
            .arg("-l")
            .arg("--color=never")
            .arg("--no-messages")
            .arg("-e")
            .arg(query);

        if settings.include_hidden {
            rg_command.arg("--hidden");
        }

        if !settings.include_mounts {
            rg_command.arg("--one-file-system");
        }

        if settings.include_symlinks {
            rg_command.arg("--follow");
        }

        for pattern in &settings.globs {
            rg_command.arg("-g").arg(format!("!{pattern}"));
        }

        let output = rg_command
            .arg(".")
            .current_dir(cwd)
            .output()
            .context("failed to execute rg for content matches")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
            let normalized = line.trim_start_matches("./");
            let rel = PathBuf::from(normalized);

            if (!settings.include_hidden && is_hidden_path(&rel)) || settings.globset.is_match(&rel)
            {
                continue;
            }

            let full = cwd.join(&rel);

            #[cfg(target_family = "unix")]
            {
                if let Some(root_dev) = root_dev {
                    if let Ok(metadata) = fs::metadata(&full) {
                        if metadata.dev() != root_dev {
                            continue;
                        }
                    }
                }
            }

            let is_dir = full.is_dir();

            map.entry(rel.clone())
                .and_modify(|candidate| candidate.content_match = true)
                .or_insert(Candidate {
                    path: rel,
                    is_dir,
                    path_match: false,
                    content_match: true,
                });
        }
    }

    let mut list: Vec<Candidate> = map.into_values().collect();
    sort_candidates(&mut list, settings.sort_mode);
    Ok(list)
}

pub fn build_search_entries(query: &str, cwd: &Path) -> Result<Vec<SearchEntry>> {
    let settings = load_settings()?;
    let highlight_re = if query.trim().is_empty() {
        None
    } else {
        Regex::new(query).ok()
    };

    // Run the filesystem walk (which finds path-name matches) and rg
    // (which finds content matches with positions) in parallel — they're
    // both I/O-heavy and independent, so doing them on separate threads
    // roughly halves the search latency on big trees. We also no longer
    // run a separate `rg -l` call inside the walk path: the data from
    // `collect_occurrences` (rg -n --column) already tells us which files
    // had content matches, so the older code was effectively running rg
    // twice over the same tree.
    let occ_handle = if query.trim().is_empty() {
        None
    } else {
        let query_owned = query.to_string();
        let cwd_owned = cwd.to_path_buf();
        let settings_owned = settings.clone();
        Some(std::thread::spawn(move || {
            collect_occurrences(&query_owned, &cwd_owned, &settings_owned)
        }))
    };

    let mut candidate_map = walk_path_candidates(query, cwd, &settings)?;

    let occurrence_map = if let Some(handle) = occ_handle {
        match handle.join() {
            Ok(Ok(map)) => map,
            Ok(Err(e)) => return Err(e),
            Err(_) => HashMap::new(),
        }
    } else {
        HashMap::new()
    };

    // Merge: any file with occurrences but no path-name match becomes a
    // content-only candidate.
    for path in occurrence_map.keys() {
        let full = cwd.join(path);
        let is_dir = full.is_dir();
        candidate_map
            .entry(path.clone())
            .and_modify(|c| c.content_match = true)
            .or_insert(Candidate {
                path: path.clone(),
                is_dir,
                path_match: false,
                content_match: true,
            });
    }

    let mut candidates: Vec<Candidate> = candidate_map.into_values().collect();
    sort_candidates(&mut candidates, settings.sort_mode);

    let mut entries = Vec::new();

    for candidate in candidates {
        let occurrences = occurrence_map.get(&candidate.path).cloned().unwrap_or_default();
        let count = occurrences.len();

        if candidate.path_match || count > 0 {
            let icon = if candidate.is_dir { "📁" } else { "📄" };
            let path_display = highlight_query_matches(
                &candidate.path.to_string_lossy(),
                highlight_re.as_ref(),
            );

            entries.push(SearchEntry {
                display: format!("{} {}", icon, path_display),
                path: candidate.path.clone(),
                line: None,
            });

            let line_width = occurrences
                .iter()
                .map(|occurrence| occurrence.line.to_string().len())
                .max()
                .unwrap_or(4)
                .max(4);

            for (index, occurrence) in occurrences.into_iter().enumerate() {
                let snippet = highlight_query_matches(&occurrence.snippet, highlight_re.as_ref());
                let count_prefix = if index == 0 {
                    format!("\x1b[33m{:>2}\x1b[0m", count)
                } else {
                    "  ".to_string()
                };

                entries.push(SearchEntry {
                    display: format!(
                        "{}   ↳ {:>width$}  {}",
                        count_prefix,
                        occurrence.line,
                        truncate_snippet(&snippet, 140),
                        width = line_width
                    ),
                    path: candidate.path.clone(),
                    line: Some(occurrence.line),
                });
            }
        }
    }

    Ok(entries)
}

/// Stream search results to stdout. In blame-sort mode, results are emitted
/// incrementally — one file at a time, in order of file last-touched — so the
/// user sees the list update as `git blame` completes for each file. Within a
/// file, occurrences are sorted by their individual line blame-date (most
/// recent first). In normal mode, results are computed and printed all at once.
pub fn run_search_streaming(query: &str, cwd: &Path) -> Result<()> {
    use std::io::Write;

    // Empty query is the startup state — fzf's `start:reload` bind invokes
    // __search with no query before the user has typed anything. The previous
    // behavior walked the entire tree and emitted every file as a row (460k+
    // rows / ~3s on a big multi-repo). fzf then had to ingest and render all
    // of those rows before it could fire the first preview, which is why
    // launching yoink "stalled" with a loading indicator for several seconds.
    //
    // yoink is a regex-search tool, not a file browser — the empty-query list
    // is never actually useful. Returning early here means the prompt opens
    // instantly and the first real reload (triggered by the user's first
    // keystroke) is the one that does work.
    if query.trim().is_empty() {
        return Ok(());
    }

    if !blame_sort_active() {
        let entries = build_search_entries(query, cwd)?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(format_search_entries(&entries).as_bytes())?;
        return Ok(());
    }

    let settings = load_settings()?;
    let highlight_re = Regex::new(query).ok();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Strategy: collect every match grouped by file (cheap), then look up
    // cached blame data for each file, then sort globally by per-line blame
    // timestamp and emit the result as ONE final list. Progress feedback for
    // the slow blame-collection step is shown in the terminal by the separate
    // `__blame_collect` subcommand (driven by the Ctrl-B fzf bind), not here.
    let grouped = collect_rg_grouped(query, cwd, &settings)?;
    let files_in_order: Vec<PathBuf> = grouped.iter().map(|(p, _)| p.clone()).collect();
    let mut by_file: HashMap<PathBuf, Vec<Occurrence>> =
        grouped.into_iter().collect();
    let total_matches: usize = by_file.values().map(|v| v.len()).sum();

    // Quick "no git repo at all" diagnostic so the user doesn't see a list
    // full of '----------' dates with no explanation. Only check the first
    // file — same answer for every match under this tree.
    if let Some(first) = files_in_order.first() {
        let abs = cwd.join(first);
        if abs.parent().and_then(find_repo_root).is_none() {
            let display = format!(
                "\x1b[1;31m[no git repo found]\x1b[0m  cwd={}  (walked up from {})",
                cwd.display(),
                first.display()
            );
            writeln!(out, "{}\t\t", display.replace('\t', "    "))?;
            out.flush()?;
        }
    }

    // Build the result vector. Blame data should already be cached by the
    // `__blame_collect` pass that runs when Ctrl-B is pressed; any cache
    // miss here (rare — only for files matched by a *new* query the user
    // typed since toggling on) will run blame inline.
    let mut all: Vec<(PathBuf, Occurrence, i64)> = Vec::with_capacity(total_matches);
    for path in &files_in_order {
        let times = blame_times_cached(cwd, path);
        if let Some(occs) = by_file.remove(path) {
            for occ in occs {
                let ts = times.get(&occ.line).copied().unwrap_or(i64::MIN);
                all.push((path.clone(), occ, ts));
            }
        }
    }

    // Global sort: most-recently-blamed line first, ties broken by path then
    // line number for deterministic output.
    all.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.to_string_lossy().cmp(&b.0.to_string_lossy()))
            .then(a.1.line.cmp(&b.1.line))
    });

    for (path, occurrence, ts) in all {
        let date = if ts != i64::MIN {
            format_unix_date(ts)
        } else {
            "----------".to_string()
        };
        let snippet = highlight_query_matches(&occurrence.snippet, highlight_re.as_ref());
        let display = format!(
            "\x1b[1;33m{}\x1b[0m  \x1b[36m{}\x1b[0m:\x1b[35m{}\x1b[0m  {}",
            date,
            path.to_string_lossy(),
            occurrence.line,
            truncate_snippet(&snippet, 140),
        );
        writeln!(
            out,
            "{}\t{}\t{}",
            display.replace('\t', "    "),
            path.to_string_lossy(),
            occurrence.line
        )?;
        out.flush()?;
    }

    Ok(())
}

/// Render a unicode progress bar of `width` columns with `done`/`total` filled.
fn progress_bar(done: usize, total: usize, width: usize) -> String {
    if total == 0 {
        return "─".repeat(width);
    }
    let filled = (done * width) / total;
    let mut bar = String::with_capacity(width * 3 + 2);
    bar.push('[');
    for i in 0..width {
        if i < filled {
            bar.push('█');
        } else {
            bar.push('░');
        }
    }
    bar.push(']');
    bar
}

/// Triggered by Ctrl-B (via fzf's `execute` action, which hands the terminal
/// over to us for the duration of the call). Runs rg → groups by file →
/// blames each file (writing to the per-session cache) while drawing a
/// single-line, self-overwriting progress bar to the terminal. When this
/// returns, fzf redraws and the subsequent `reload` runs `__search` against
/// the now-warm cache, so the user sees a single fully-sorted list with no
/// progress noise.
pub fn run_blame_collect(query: &str, cwd: &Path) -> Result<()> {
    // Only do anything when we are actually entering blame mode. The Ctrl-B
    // bind toggles state *before* invoking us, so blame_sort_active() returns
    // true iff the new state is blame mode.
    if !blame_sort_active() {
        return Ok(());
    }

    // Start with a fresh cache so toggling blame on always reflects the
    // current state of the working tree. (Subsequent reloads from typing
    // re-use this cache for the rest of the session.)
    crate::blame::clear_blame_cache();

    let stderr = std::io::stderr();
    let mut term = stderr.lock();

    // Clear screen, home cursor, hide cursor.
    write!(term, "\x1b[2J\x1b[H\x1b[?25l")?;
    writeln!(
        term,
        "\x1b[1;36m🔍 yoink — preparing blame-sorted view\x1b[0m"
    )?;
    if !query.trim().is_empty() {
        writeln!(term, "  query: \x1b[1m{query}\x1b[0m")?;
    }
    writeln!(term)?;
    term.flush()?;

    if query.trim().is_empty() {
        write!(term, "\x1b[?25h")?;
        term.flush()?;
        return Ok(());
    }

    let settings = load_settings()?;
    let by_file = match collect_rg_grouped(query, cwd, &settings) {
        Ok(v) => v,
        Err(e) => {
            writeln!(term, "\x1b[1;31m✗ rg failed: {e}\x1b[0m")?;
            write!(term, "\x1b[?25h")?;
            term.flush()?;
            return Ok(());
        }
    };

    let total = by_file.len();
    let total_matches: usize = by_file.iter().map(|(_, v)| v.len()).sum();

    if total == 0 {
        writeln!(
            term,
            "\x1b[2;37mNo matches for this query — nothing to blame.\x1b[0m"
        )?;
        write!(term, "\x1b[?25h")?;
        term.flush()?;
        return Ok(());
    }

    writeln!(
        term,
        "  \x1b[1;33m{total_matches}\x1b[0m matches across \x1b[1;33m{total}\x1b[0m files"
    )?;
    writeln!(term)?;
    term.flush()?;

    let start = std::time::Instant::now();
    let bar_width = 36;
    for (idx, (path, _occs)) in by_file.iter().enumerate() {
        let _ = blame_times_cached(cwd, path);
        let done = idx + 1;
        let bar = progress_bar(done, total, bar_width);
        let pct = (done * 100) / total;
        let label = path.to_string_lossy();
        let truncated: String = label.chars().take(60).collect();
        // \r returns to start of line; \x1b[K clears to end of line so a
        // shorter path doesn't leave trailing characters from a previous
        // longer one. The whole status fits on one updating line.
        write!(
            term,
            "\r  \x1b[1;33m⏳\x1b[0m {bar} \x1b[1;36m{pct:>3}%\x1b[0m  \x1b[2;37m{done}/{total}\x1b[0m  \x1b[2;37m{truncated}\x1b[0m\x1b[K"
        )?;
        term.flush()?;
    }

    let elapsed = start.elapsed();
    writeln!(term)?;
    writeln!(
        term,
        "  \x1b[1;32m✓\x1b[0m blamed \x1b[1;33m{total}\x1b[0m files in \x1b[1;33m{:.1}s\x1b[0m — sorting…",
        elapsed.as_secs_f64()
    )?;
    // Show cursor again before yielding the terminal back to fzf.
    write!(term, "\x1b[?25h")?;
    term.flush()?;

    Ok(())
}

/// Run rg with the given query/settings and collect all matches grouped by
/// file. Used by both `__search` blame mode and `__blame_collect`.
fn collect_rg_grouped(
    query: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<Vec<(PathBuf, Vec<Occurrence>)>> {
    let mut rg_cmd = Command::new("rg");
    rg_cmd
        .arg("-n")
        .arg("--column")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--no-messages")
        .arg("-e")
        .arg(query);
    if settings.include_hidden {
        rg_cmd.arg("--hidden");
    }
    if !settings.include_mounts {
        rg_cmd.arg("--one-file-system");
    }
    if settings.include_symlinks {
        rg_cmd.arg("--follow");
    }
    for pattern in &settings.globs {
        rg_cmd.arg("-g").arg(format!("!{pattern}"));
    }
    let mut child = rg_cmd
        .arg(".")
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn rg")?;

    let rg_stdout = child.stdout.take().context("rg stdout pipe missing")?;
    let reader = BufReader::new(rg_stdout);

    let mut order: Vec<PathBuf> = Vec::new();
    let mut by_file: HashMap<PathBuf, Vec<Occurrence>> = HashMap::new();

    for raw in reader.lines().map_while(Result::ok) {
        if raw.trim().is_empty() {
            continue;
        }
        let mut parts = raw.splitn(4, ':');
        let path_str = parts.next().unwrap_or("");
        let Some(line_str) = parts.next() else { continue };
        let Some(col_str) = parts.next() else { continue };
        let snippet = parts.next().unwrap_or_default();
        let Ok(line) = line_str.parse::<usize>() else {
            continue;
        };
        let Ok(column) = col_str.parse::<usize>() else {
            continue;
        };
        let path = PathBuf::from(path_str.trim_start_matches("./"));
        let entry = by_file.entry(path.clone()).or_insert_with(|| {
            order.push(path.clone());
            Vec::new()
        });
        entry.push(Occurrence {
            line,
            column,
            snippet: snippet.replace('\t', " ").trim().to_string(),
        });
    }
    let _ = child.wait();

    let mut result = Vec::with_capacity(order.len());
    for path in order {
        if let Some(occs) = by_file.remove(&path) {
            result.push((path, occs));
        }
    }
    Ok(result)
}

pub fn format_search_entries(entries: &[SearchEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        let line = entry.line.map(|v| v.to_string()).unwrap_or_default();
        out.push_str(&entry.display.replace('\t', "    "));
        out.push('\t');
        out.push_str(&entry.path.to_string_lossy());
        out.push('\t');
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn truncate_snippet(snippet: &str, max_chars: usize) -> String {
    if snippet.chars().count() <= max_chars {
        return snippet.to_string();
    }

    let mut out = String::new();
    for (idx, ch) in snippet.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn highlight_query_matches(text: &str, re: Option<&Regex>) -> String {
    let Some(re) = re else {
        return text.to_string();
    };

    let mut out = String::new();
    let mut last = 0usize;

    for matched in re.find_iter(text) {
        if matched.start() > last {
            out.push_str(&text[last..matched.start()]);
        }
        out.push_str("\x1b[1;36m");
        out.push_str(matched.as_str());
        out.push_str("\x1b[0m");
        last = matched.end();
    }

    if last < text.len() {
        out.push_str(&text[last..]);
    }

    out
}

fn sort_candidates(candidates: &mut [Candidate], sort_mode: SortMode) {
    match sort_mode {
        SortMode::Depth => {
            candidates.sort_by_key(|candidate| {
                (
                    path_depth(&candidate.path),
                    candidate.path.to_string_lossy().to_string(),
                )
            });
        }
        SortMode::Alphabetical => {
            candidates.sort_by_key(|candidate| candidate.path.to_string_lossy().to_string());
        }
    }
}

fn collect_occurrences(
    query: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<HashMap<PathBuf, Vec<Occurrence>>> {
    let mut rg_command = Command::new("rg");
    rg_command
        .arg("-n")
        .arg("--column")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--no-messages")
        .arg("-e")
        .arg(query);

    if settings.include_hidden {
        rg_command.arg("--hidden");
    }

    if !settings.include_mounts {
        rg_command.arg("--one-file-system");
    }

    if settings.include_symlinks {
        rg_command.arg("--follow");
    }

    for pattern in &settings.globs {
        rg_command.arg("-g").arg(format!("!{pattern}"));
    }

    let output = rg_command
        .arg(".")
        .current_dir(cwd)
        .output()
        .context("failed to execute rg for detailed occurrences")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map: HashMap<PathBuf, Vec<Occurrence>> = HashMap::new();

    for raw_line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = raw_line.splitn(4, ':');
        let raw_path = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let raw_line_num = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let raw_column = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let raw_snippet = parts.next().unwrap_or_default();

        let path = PathBuf::from(raw_path.trim_start_matches("./"));
        let line_num = match raw_line_num.parse::<usize>() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let column = match raw_column.parse::<usize>() {
            Ok(value) => value,
            Err(_) => continue,
        };

        map.entry(path).or_default().push(Occurrence {
            line: line_num,
            column,
            snippet: raw_snippet.replace('\t', " ").trim().to_string(),
        });
    }

    for occurrences in map.values_mut() {
        occurrences.sort_by_key(|occurrence| (occurrence.line, occurrence.column));
    }

    Ok(map)
}
