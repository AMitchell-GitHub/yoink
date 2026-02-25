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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub is_dir: bool,
    pub path_match: bool,
    pub content_match: bool,
}

impl Candidate {
    pub fn tag(&self) -> &'static str {
        match (self.path_match, self.content_match) {
            (true, true) => "BOTH",
            (true, false) => "PATH",
            (false, true) => "TEXT",
            (false, false) => "PATH",
        }
    }
}

#[derive(Debug)]
struct YoinkSettings {
    include_hidden: bool,
    include_mounts: bool,
    include_symlinks: bool,
    globset: GlobSet,
    globs: Vec<String>,
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
    list.sort_by_key(candidate_sort_key);
    Ok(list)
}

pub fn format_candidates(candidates: &[Candidate]) -> String {
    let mut out = String::new();
    for candidate in candidates {
        out.push_str(candidate.tag());
        out.push('\t');
        out.push_str(&candidate.path.to_string_lossy());
        out.push('\n');
    }
    out
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn candidate_sort_key(candidate: &Candidate) -> (usize, String) {
    (
        path_depth(&candidate.path),
        candidate.path.to_string_lossy().to_string(),
    )
}
