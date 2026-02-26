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

const DEFAULT_IGNORE_GLOBS: &[&str] = &[".git/**", "node_modukes/**"];

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

#[derive(Debug)]
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

pub fn build_candidates(query: &str, cwd: &Path) -> Result<Vec<Candidate>> {
    let mut map: HashMap<PathBuf, Candidate> = HashMap::new();
    let settings = load_settings()?;

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
    let candidates = build_candidates(query, cwd)?;
    let highlight_re = if query.trim().is_empty() {
        None
    } else {
        Regex::new(query).ok()
    };

    let occurrence_map = if query.trim().is_empty() {
        HashMap::new()
    } else {
        collect_occurrences(query, cwd, &settings)?
    };

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

            let display = format!("{} {}", icon, path_display);

            entries.push(SearchEntry {
                display,
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
