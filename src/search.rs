use crate::actions::Action;
use crate::blame::{
    blame_sort_active, blame_times_cached, find_repo_root, format_unix_date, BlameOrder,
};
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

#[cfg(target_family = "unix")]
use std::os::unix::fs::MetadataExt;

const DEFAULT_IGNORE_GLOBS: &[&str] = &["**/.git", "**/node_modules"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Glob,
    Regex,
}

impl SearchMode {
    pub fn token(self) -> &'static str {
        match self {
            SearchMode::Glob => "glob",
            SearchMode::Regex => "regex",
        }
    }

    fn from_token(value: &str) -> Option<SearchMode> {
        match value.trim().to_ascii_lowercase().as_str() {
            "glob" => Some(SearchMode::Glob),
            "regex" => Some(SearchMode::Regex),
            _ => None,
        }
    }
}

/// Unified sort selector — one config key, four values. `Depth` and
/// `Alphabetical` are pure path orderings; `BlameYoung` and `BlameOld`
/// reorder by per-line `git blame` timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Depth,
    Alphabetical,
    BlameYoung,
    BlameOld,
}

impl Sort {
    pub fn token(self) -> &'static str {
        match self {
            Sort::Depth => "depth",
            Sort::Alphabetical => "alphabetical",
            Sort::BlameYoung => "blame_young",
            Sort::BlameOld => "blame_old",
        }
    }

    fn from_token(value: &str) -> Option<Sort> {
        match value.trim().to_ascii_lowercase().as_str() {
            "depth" => Some(Sort::Depth),
            "alphabetical" | "alpha" => Some(Sort::Alphabetical),
            "blame_young" | "blame-young" | "blame_youngest" => Some(Sort::BlameYoung),
            "blame_old" | "blame-old" | "blame_oldest" => Some(Sort::BlameOld),
            _ => None,
        }
    }
}

/// Every configurable keybind, result-action *and* query-editing alike, in the
/// order it appears in the config. A key does nothing unless it's listed here —
/// there are no built-in defaults for `ctrl-*` chords. The only fixed keys are
/// the reserved built-ins (F1–F5 / Esc / Enter / Ctrl-C / nav).
pub type Binds = Vec<(String, Action)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub is_dir: bool,
    pub path_match: bool,
    pub content_match: bool,
}

#[derive(Debug, Clone)]
pub struct YoinkSettings {
    include_hidden: bool,
    include_mounts: bool,
    include_symlinks: bool,
    globset: GlobSet,
    globs: Vec<String>,
    pub search_mode: SearchMode,
    pub case_sensitive: bool,
    pub sort: Sort,
    /// Whether to check GitHub for a newer release on startup. Default true.
    pub update_check: bool,
    pub binds: Binds,
    /// Path the settings were loaded from (or would be written to if config
    /// doesn't exist yet). `None` if no $HOME is available.
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub display: String,
    pub path: PathBuf,
    pub line: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Occurrence {
    pub line: usize,
    pub column: usize,
    pub snippet: String,
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

/// Resolve the active config path. Single source of truth — there is no
/// legacy fallback. Read and write target are the same.
///   1. `$YOINK_CONFIG_PATH` (env override, primarily for tests)
///   2. `$HOME/.yoink-config`
///
/// Returns `None` only if there's no `$HOME` and no override.
pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = env::var_os("YOINK_CONFIG_PATH") {
        return Some(PathBuf::from(p));
    }
    let home = env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".yoink-config"))
}

pub fn load_settings() -> Result<YoinkSettings> {
    let mut include_hidden = false;
    let mut include_mounts = false;
    let mut include_symlinks = false;
    let mut search_mode = SearchMode::Glob;
    let mut case_sensitive = false;
    let mut sort = Sort::Depth;
    let mut update_check = true;
    let mut binds: Binds = Vec::new();
    // Globs default to the built-in safe set when no config exists at all
    // (e.g. the `yoink __search` headless path with no $HOME). Once a config
    // file is read, *its* globs are the source of truth — even if it lists
    // none, we honor that.
    let mut globs: Vec<String> = Vec::new();
    let mut config_was_read = false;

    let source = config_path();

    if let Some(ref config_file) = source {
        if config_file.exists() {
            config_was_read = true;
            let content = fs::read_to_string(config_file)
                .with_context(|| format!("failed to read {}", config_file.display()))?;

            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                if let Some((raw_key, raw_value)) = trimmed.split_once('=') {
                    let key = raw_key.trim().to_ascii_lowercase();
                    let value = raw_value.trim();

                    if let Some(bind_key) = key.strip_prefix("bind.") {
                        if let Some(action) = Action::from_token(value) {
                            binds.push((bind_key.to_string(), action));
                        } else {
                            eprintln!(
                                "yoink config: unknown action `{value}` for bind `{bind_key}` in {}",
                                config_file.display()
                            );
                        }
                        continue;
                    }

                    match key.as_str() {
                        "include_hidden" => {
                            include_hidden = parse_bool_setting(value).with_context(|| {
                                format!(
                                    "invalid include_hidden value in {}: {value}",
                                    config_file.display()
                                )
                            })?;
                            continue;
                        }
                        "include_mounts" => {
                            include_mounts = parse_bool_setting(value).with_context(|| {
                                format!(
                                    "invalid include_mounts value in {}: {value}",
                                    config_file.display()
                                )
                            })?;
                            continue;
                        }
                        "include_symlinks" => {
                            include_symlinks = parse_bool_setting(value).with_context(|| {
                                format!(
                                    "invalid include_symlinks value in {}: {value}",
                                    config_file.display()
                                )
                            })?;
                            continue;
                        }
                        "search_mode" => {
                            search_mode = SearchMode::from_token(value).with_context(|| {
                                format!(
                                    "invalid search_mode value in {}: {value} (expected glob|regex)",
                                    config_file.display()
                                )
                            })?;
                            continue;
                        }
                        "case_sensitive" => {
                            case_sensitive = parse_bool_setting(value).with_context(|| {
                                format!(
                                    "invalid case_sensitive value in {}: {value}",
                                    config_file.display()
                                )
                            })?;
                            continue;
                        }
                        "sort" => {
                            sort = Sort::from_token(value).with_context(|| {
                                format!(
                                    "invalid sort value in {}: {value} (expected depth|alphabetical|blame_young|blame_old)",
                                    config_file.display()
                                )
                            })?;
                            continue;
                        }
                        "update_check" => {
                            update_check = parse_bool_setting(value).with_context(|| {
                                format!(
                                    "invalid update_check value in {}: {value}",
                                    config_file.display()
                                )
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

    if !config_was_read {
        // No config file yet — fall back to the safe built-in ignore set so
        // a stray `yoink __search` doesn't walk into `.git` and explode.
        globs.extend(DEFAULT_IGNORE_GLOBS.iter().map(|s| s.to_string()));
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in &globs {
        builder.add(Glob::new(pattern).with_context(|| format!("invalid ignore glob: {pattern}"))?);
    }

    let globset = builder.build().context("failed building ignore glob set")?;
    Ok(YoinkSettings {
        include_hidden,
        include_mounts,
        include_symlinks,
        globset,
        globs,
        search_mode,
        case_sensitive,
        sort,
        update_check,
        binds,
        source_path: source,
    })
}

/// Translate the user's query into a single regex string used everywhere —
/// the rust path-walk matcher, the rg content invocation, and the highlight
/// regex. In regex mode this is the query verbatim. In glob mode the query
/// is glob-translated to a regex with anchors stripped (so it matches
/// substring-style, like the rest of the pipeline). Case-insensitivity is
/// applied with a `(?i)` prefix so a single string carries the flag through
/// to both `regex::Regex` and `rg`.
pub fn effective_pattern(query: &str, mode: SearchMode, case_sensitive: bool) -> Result<String> {
    if query.is_empty() {
        return Ok(String::new());
    }

    let raw = match mode {
        SearchMode::Regex => query.to_string(),
        SearchMode::Glob => glob_to_regex(query)?,
    };

    if case_sensitive {
        Ok(raw)
    } else {
        Ok(format!("(?i){raw}"))
    }
}

/// Convert a glob query into an unanchored regex. Built on top of
/// `globset::Glob::regex()` which emits an anchored, byte-mode regex; we
/// strip the leading `(?-u)` flag (we match against UTF-8 path strings and
/// `rg` output lines, so the regex crate's default unicode mode is correct
/// and required — combining `(?-u)` with `.*` triggers a "pattern can
/// match invalid UTF-8" error against `Regex::new`) and the anchors so the
/// resulting pattern matches substring-style (consistent with regex mode,
/// where `rg` and `Regex::is_match` are unanchored by default).
fn glob_to_regex(query: &str) -> Result<String> {
    let glob = Glob::new(query).with_context(|| format!("invalid glob query: {query}"))?;
    let mut re = glob.regex().to_string();
    if let Some(rest) = re.strip_prefix("(?-u)") {
        re = rest.to_string();
    }
    if re.starts_with('^') {
        re.remove(0);
    }
    if re.ends_with('$') {
        re.pop();
    }
    Ok(re)
}

fn walk_path_candidates(
    pattern_re: Option<&Regex>,
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

        let is_match = match pattern_re {
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

/// Apply the file-walk options + ignore globs that are common across every
/// `rg` invocation. Centralized so search_mode/case stay in lockstep across
/// all three rg call sites.
fn apply_rg_common_args(cmd: &mut Command, settings: &YoinkSettings) {
    if settings.include_hidden {
        cmd.arg("--hidden");
    }
    if !settings.include_mounts {
        cmd.arg("--one-file-system");
    }
    if settings.include_symlinks {
        cmd.arg("--follow");
    }
    for pattern in &settings.globs {
        cmd.arg("-g").arg(format!("!{pattern}"));
    }
}

/// Kept as a public API for the integration tests in tests/search.rs.
#[allow(dead_code)]
pub fn build_candidates(query: &str, cwd: &Path) -> Result<Vec<Candidate>> {
    let settings = load_settings()?;
    let effective = effective_pattern(query, settings.search_mode, settings.case_sensitive)?;
    let pattern_re = if effective.is_empty() {
        None
    } else {
        Some(Regex::new(&effective).with_context(|| format!("invalid regex query: {effective}"))?)
    };
    let mut map = walk_path_candidates(pattern_re.as_ref(), cwd, &settings)?;

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

    if !effective.is_empty() {
        let mut rg_command = Command::new("rg");
        rg_command
            .arg("-l")
            .arg("--color=never")
            .arg("--no-messages")
            .arg("-e")
            .arg(&effective);
        apply_rg_common_args(&mut rg_command, &settings);

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
    sort_candidates(&mut list, settings.sort);
    Ok(list)
}

pub fn build_search_entries(
    query: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<Vec<SearchEntry>> {
    let effective = effective_pattern(query, settings.search_mode, settings.case_sensitive)?;

    let highlight_re = if effective.is_empty() {
        None
    } else {
        Regex::new(&effective).ok()
    };

    let occ_handle = if effective.is_empty() {
        None
    } else {
        let pattern = effective.clone();
        let cwd_owned = cwd.to_path_buf();
        let settings_owned = settings.clone();
        Some(std::thread::spawn(move || {
            collect_occurrences(&pattern, &cwd_owned, &settings_owned)
        }))
    };

    let walk_pattern = if effective.is_empty() {
        None
    } else {
        Some(Regex::new(&effective).with_context(|| format!("invalid regex query: {effective}"))?)
    };
    let mut candidate_map = walk_path_candidates(walk_pattern.as_ref(), cwd, settings)?;

    let occurrence_map = if let Some(handle) = occ_handle {
        match handle.join() {
            Ok(Ok(map)) => map,
            Ok(Err(e)) => return Err(e),
            Err(_) => HashMap::new(),
        }
    } else {
        HashMap::new()
    };

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
    sort_candidates(&mut candidates, settings.sort);

    let mut entries = Vec::new();

    for candidate in candidates {
        let occurrences = occurrence_map
            .get(&candidate.path)
            .cloned()
            .unwrap_or_default();
        let count = occurrences.len();

        if candidate.path_match || count > 0 {
            let icon = if candidate.is_dir { "📁" } else { "📄" };
            let path_display =
                highlight_query_matches(&candidate.path.to_string_lossy(), highlight_re.as_ref());

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

/// Build the same entries as `build_search_entries` but globally sorted by
/// per-line `git blame` timestamp. Order (youngest first vs oldest first) is
/// taken from the active config. Used by the TUI when a blame sort is on.
///
/// Blame lookups go through `blame_times_cached`, which is itself
/// cache-then-miss-and-warm — so calling this repeatedly during a session
/// reuses any blame data warmed on a previous invocation. Switching
/// young↔old never re-runs `git blame`; only the in-memory sort flips.
pub fn build_blame_sorted_entries(
    query: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<Vec<SearchEntry>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let effective = effective_pattern(query, settings.search_mode, settings.case_sensitive)?;
    let highlight_re = if effective.is_empty() {
        None
    } else {
        Regex::new(&effective).ok()
    };

    let grouped = collect_rg_grouped(&effective, cwd, settings)?;
    let files_in_order: Vec<PathBuf> = grouped.iter().map(|(p, _)| p.clone()).collect();
    let mut by_file: HashMap<PathBuf, Vec<Occurrence>> = grouped.into_iter().collect();
    let total_matches: usize = by_file.values().map(|v| v.len()).sum();

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

    let order = BlameOrder::from_sort(settings.sort);
    all.sort_by(|a, b| {
        let primary = match order {
            BlameOrder::Youngest => b.2.cmp(&a.2),
            BlameOrder::Oldest => a.2.cmp(&b.2),
        };
        primary
            .then_with(|| a.0.to_string_lossy().cmp(&b.0.to_string_lossy()))
            .then(a.1.line.cmp(&b.1.line))
    });

    let mut entries = Vec::with_capacity(all.len());
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
        entries.push(SearchEntry {
            display,
            path: path.clone(),
            line: Some(occurrence.line),
        });
    }
    Ok(entries)
}

/// Stream search results to stdout. In blame-sort mode, the global list is
/// sorted by per-line blame timestamp (youngest or oldest first depending on
/// the configured `sort`).
pub fn run_search_streaming(query: &str, cwd: &Path) -> Result<()> {
    if query.trim().is_empty() {
        return Ok(());
    }

    let settings = load_settings()?;

    if !blame_sort_active(&settings) {
        let entries = build_search_entries(query, cwd, &settings)?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(format_search_entries(&entries).as_bytes())?;
        return Ok(());
    }

    let effective = effective_pattern(query, settings.search_mode, settings.case_sensitive)?;
    let highlight_re = Regex::new(&effective).ok();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let grouped = collect_rg_grouped(&effective, cwd, &settings)?;
    let files_in_order: Vec<PathBuf> = grouped.iter().map(|(p, _)| p.clone()).collect();
    let mut by_file: HashMap<PathBuf, Vec<Occurrence>> = grouped.into_iter().collect();
    let total_matches: usize = by_file.values().map(|v| v.len()).sum();

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

    let order = BlameOrder::from_sort(settings.sort);
    all.sort_by(|a, b| {
        let primary = match order {
            BlameOrder::Youngest => b.2.cmp(&a.2),
            BlameOrder::Oldest => a.2.cmp(&b.2),
        };
        primary
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

/// Public façade around the internal grouped-occurrence collector, used by
/// the TUI's blame warm-up worker thread. The returned vector contains one
/// entry per file with matches; the `Occurrence` interior is intentionally
/// opaque to callers (they only need the path list to drive the cache walk).
pub fn collect_rg_grouped_public(
    pattern: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<Vec<(PathBuf, usize)>> {
    let grouped = collect_rg_grouped(pattern, cwd, settings)?;
    Ok(grouped
        .into_iter()
        .map(|(p, occs)| (p, occs.len()))
        .collect())
}

/// Collect files and directories whose *name or path* matches the query, with
/// no regard to file contents. Returns `(relative_path, is_dir)` pairs sorted
/// by path. Used by the headless CLI to surface name-only hits alongside the
/// content matches from `collect_rg_grouped`.
pub fn collect_path_matches(
    query: &str,
    cwd: &Path,
    settings: &YoinkSettings,
) -> Result<Vec<(PathBuf, bool)>> {
    let effective = effective_pattern(query, settings.search_mode, settings.case_sensitive)?;
    if effective.is_empty() {
        return Ok(Vec::new());
    }
    let pattern_re =
        Regex::new(&effective).with_context(|| format!("invalid regex query: {effective}"))?;
    let map = walk_path_candidates(Some(&pattern_re), cwd, settings)?;
    let mut out: Vec<(PathBuf, bool)> = map
        .into_values()
        .filter(|candidate| candidate.path_match)
        .map(|candidate| (candidate.path, candidate.is_dir))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Run `rg` for content matches and return them grouped per file, in the
/// order `rg` first emitted each file. Each `Occurrence` carries the 1-indexed
/// line, column, and a trimmed snippet. Public so the headless CLI can build
/// structured (JSON/markdown/text) output from the same matcher the TUI uses.
pub fn collect_rg_grouped(
    pattern: &str,
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
        .arg(pattern);
    apply_rg_common_args(&mut rg_cmd, settings);

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
        let Some(line_str) = parts.next() else {
            continue;
        };
        let Some(col_str) = parts.next() else {
            continue;
        };
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

fn sort_candidates(candidates: &mut [Candidate], sort: Sort) {
    match sort {
        Sort::Alphabetical => {
            candidates.sort_by_key(|candidate| candidate.path.to_string_lossy().to_string());
        }
        // Depth — and the blame variants too, which never reach this
        // function from the regular search path (the TUI dispatches them to
        // `build_blame_sorted_entries`). Treat them as Depth here so we
        // remain coherent if reached.
        _ => {
            candidates.sort_by_key(|candidate| {
                (
                    path_depth(&candidate.path),
                    candidate.path.to_string_lossy().to_string(),
                )
            });
        }
    }
}

fn collect_occurrences(
    pattern: &str,
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
        .arg(pattern);
    apply_rg_common_args(&mut rg_command, settings);

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
