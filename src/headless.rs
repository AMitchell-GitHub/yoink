//! Non-interactive ("headless") search. Triggered by `--output`, this
//! runs the same matcher the TUI uses (`search::collect_rg_grouped` for content
//! plus `search::collect_path_matches` for name hits), enriches each match with
//! ±N lines of surrounding code and optional git-blame data, then serializes
//! the lot as JSON, JSONL, Markdown, or plain text.
//!
//! The intent is to make yoink's results easy for another program — a shell
//! script or an LLM — to consume, and to give users a second, copy-pasteable
//! way to export search results.

use crate::blame::{
    blame_for_file_cached, blame_sort_active, file_last_touched, format_unix_date,
    SESSION_CACHE_ENV,
};
use crate::cli::OutputFormat;
use crate::search::{
    collect_path_matches, collect_rg_grouped, effective_pattern, load_settings, SearchMode, Sort,
};
use anyhow::{bail, Result};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Everything the headless path needs, assembled from the parsed CLI in
/// `main`. The `mode`/`sort`/`case_sensitive` overrides are `None` when the
/// user didn't pass the corresponding flag (config value is then used).
pub struct HeadlessOptions {
    pub query: String,
    pub format: OutputFormat,
    pub mode: Option<SearchMode>,
    pub sort: Option<Sort>,
    pub case_sensitive: Option<bool>,
    pub context: usize,
    pub max_results: Option<usize>,
    pub blame: bool,
    pub content_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Content,
    Path,
}

impl Kind {
    fn token(self) -> &'static str {
        match self {
            Kind::Content => "content",
            Kind::Path => "path",
        }
    }
}

#[derive(Debug, Clone)]
struct BlameInfo {
    date: String,
    timestamp: i64,
    author: String,
    sha: String,
}

#[derive(Debug, Clone)]
struct MatchRecord {
    kind: Kind,
    path: PathBuf,
    is_dir: bool,
    line: Option<usize>,
    column: Option<usize>,
    match_line: Option<String>,
    context_before: Vec<String>,
    context_after: Vec<String>,
    context_start_line: Option<usize>,
    blame: Option<BlameInfo>,
    /// Sort key for blame orderings; `None` when blame data is unavailable.
    sort_ts: Option<i64>,
}

impl MatchRecord {
    fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// Run a one-shot search and print the results in the requested format. Never
/// launches the TUI.
pub fn run_headless(opts: HeadlessOptions, cwd: &Path) -> Result<()> {
    if opts.query.trim().is_empty() {
        bail!("a non-empty query is required for --output (try: yoink 'pattern' -o json)");
    }

    let mut settings = load_settings()?;
    if let Some(mode) = opts.mode {
        settings.search_mode = mode;
    }
    if let Some(sort) = opts.sort {
        settings.sort = sort;
    }
    if let Some(case_sensitive) = opts.case_sensitive {
        settings.case_sensitive = case_sensitive;
    }

    // Give blame lookups a per-run on-disk cache so a file blamed for one
    // match isn't re-blamed for the next match in the same file. Best-effort:
    // if the temp dir can't be created we just run without caching.
    let cache_dir = setup_cache_dir();

    let effective = effective_pattern(&opts.query, settings.search_mode, settings.case_sensitive)?;
    let want_blame = opts.blame || blame_sort_active(&settings);

    let mut records: Vec<MatchRecord> = Vec::new();

    // Content matches: one record per occurrence, with surrounding context.
    let grouped = collect_rg_grouped(&effective, cwd, &settings)?;
    let content_paths: HashSet<PathBuf> = grouped.iter().map(|(path, _)| path.clone()).collect();

    for (path, occurrences) in &grouped {
        let lines = read_file_lines(&cwd.join(path));
        let blame_map = if want_blame {
            Some(blame_for_file_cached(cwd, path))
        } else {
            None
        };

        for occurrence in occurrences {
            let (context_before, context_after, context_start_line, match_line) =
                build_window(lines.as_deref(), occurrence.line, opts.context);
            // Fall back to rg's trimmed snippet if we couldn't read the file
            // line (e.g. it changed under us between rg and our read).
            let match_line = match_line.or_else(|| Some(occurrence.snippet.clone()));
            let blame = blame_map
                .as_ref()
                .and_then(|map| map.get(&occurrence.line))
                .map(to_blame_info);
            let sort_ts = blame.as_ref().map(|info| info.timestamp);

            records.push(MatchRecord {
                kind: Kind::Content,
                path: path.clone(),
                is_dir: false,
                line: Some(occurrence.line),
                column: Some(occurrence.column),
                match_line,
                context_before,
                context_after,
                context_start_line,
                blame,
                sort_ts,
            });
        }
    }

    // Name matches: files/dirs whose path matched the query. Skip files that
    // already surfaced through a content match — they'd just be duplicate
    // noise — but always keep directories (they never have content matches).
    if !opts.content_only {
        for (path, is_dir) in collect_path_matches(&opts.query, cwd, &settings)? {
            if !is_dir && content_paths.contains(&path) {
                continue;
            }
            let (blame, sort_ts) = if want_blame {
                match file_last_touched(cwd, &path) {
                    Some((timestamp, author)) => (
                        Some(BlameInfo {
                            date: format_unix_date(timestamp),
                            timestamp,
                            author,
                            sha: String::new(),
                        }),
                        Some(timestamp),
                    ),
                    None => (None, None),
                }
            } else {
                (None, None)
            };

            records.push(MatchRecord {
                kind: Kind::Path,
                path,
                is_dir,
                line: None,
                column: None,
                match_line: None,
                context_before: Vec::new(),
                context_after: Vec::new(),
                context_start_line: None,
                blame,
                sort_ts,
            });
        }
    }

    sort_records(&mut records, settings.sort);

    let total = records.len();
    let truncated = opts.max_results.is_some_and(|max| total > max);
    if let Some(max) = opts.max_results {
        records.truncate(max);
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let rendered = match opts.format {
        OutputFormat::Json => render_json(&opts, &settings, cwd, &records, total, truncated),
        OutputFormat::Jsonl => render_jsonl(&records),
        OutputFormat::Markdown => {
            render_markdown(&opts, &settings, cwd, &records, total, truncated)
        }
        OutputFormat::Text => render_text(&records),
    };
    out.write_all(rendered.as_bytes())?;

    // Best-effort cleanup of the per-run blame cache.
    if let Some(dir) = cache_dir {
        let _ = fs::remove_dir_all(&dir);
    }

    Ok(())
}

fn to_blame_info(info: &crate::blame::LineBlame) -> BlameInfo {
    BlameInfo {
        date: format_unix_date(info.timestamp),
        timestamp: info.timestamp,
        author: info.author.clone(),
        sha: info.sha.clone(),
    }
}

/// Create and register a private temp directory for this run's blame cache.
/// Returns the path so the caller can remove it on exit. `None` if creation
/// fails — blame still works, just uncached.
fn setup_cache_dir() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("yoink-cli-{}", std::process::id()));
    fs::create_dir_all(&dir).ok()?;
    std::env::set_var(SESSION_CACHE_ENV, &dir);
    Some(dir)
}

fn read_file_lines(abs: &Path) -> Option<Vec<String>> {
    fs::read_to_string(abs)
        .ok()
        .map(|content| content.lines().map(|line| line.to_string()).collect())
}

/// Build the context window around a 1-indexed `line`. Returns
/// `(before, after, context_start_line, match_line)`. `context_start_line` is
/// the 1-indexed line number of the first `before` line (or the match line
/// itself when there is nothing before it).
fn build_window(
    lines: Option<&[String]>,
    line: usize,
    context: usize,
) -> (Vec<String>, Vec<String>, Option<usize>, Option<String>) {
    let Some(lines) = lines else {
        return (Vec::new(), Vec::new(), None, None);
    };
    if line == 0 || line > lines.len() {
        return (Vec::new(), Vec::new(), None, None);
    }
    let index = line - 1;
    let start = index.saturating_sub(context);
    let end = (index + 1 + context).min(lines.len());
    let before = lines[start..index].to_vec();
    let after = lines[index + 1..end].to_vec();
    let match_line = Some(lines[index].clone());
    (before, after, Some(start + 1), match_line)
}

fn sort_records(records: &mut [MatchRecord], sort: Sort) {
    match sort {
        Sort::Alphabetical => {
            records.sort_by(|a, b| a.path_str().cmp(&b.path_str()).then(a.line.cmp(&b.line)))
        }
        Sort::Depth => records.sort_by(|a, b| {
            let depth = |record: &MatchRecord| record.path.components().count();
            depth(a)
                .cmp(&depth(b))
                .then_with(|| a.path_str().cmp(&b.path_str()))
                .then(a.line.cmp(&b.line))
        }),
        Sort::BlameYoung | Sort::BlameOld => {
            // Records without blame data sort last in either direction.
            records.sort_by(|a, b| {
                let primary = match (a.sort_ts, b.sort_ts) {
                    (Some(x), Some(y)) => {
                        if matches!(sort, Sort::BlameYoung) {
                            y.cmp(&x)
                        } else {
                            x.cmp(&y)
                        }
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                };
                primary
                    .then_with(|| a.path_str().cmp(&b.path_str()))
                    .then(a.line.cmp(&b.line))
            });
        }
    }
}

// --- Serialization -------------------------------------------------------

fn case_token(case_sensitive: bool) -> &'static str {
    if case_sensitive {
        "sensitive"
    } else {
        "insensitive"
    }
}

fn render_json(
    opts: &HeadlessOptions,
    settings: &crate::search::YoinkSettings,
    cwd: &Path,
    records: &[MatchRecord],
    total: usize,
    truncated: bool,
) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"query\": {},\n", json_string(&opts.query)));
    out.push_str(&format!(
        "  \"mode\": {},\n",
        json_string(settings.search_mode.token())
    ));
    out.push_str(&format!(
        "  \"sort\": {},\n",
        json_string(settings.sort.token())
    ));
    out.push_str(&format!(
        "  \"case\": {},\n",
        json_string(case_token(settings.case_sensitive))
    ));
    out.push_str(&format!(
        "  \"root\": {},\n",
        json_string(&cwd.to_string_lossy())
    ));
    out.push_str(&format!("  \"context_lines\": {},\n", opts.context));
    out.push_str(&format!("  \"count\": {},\n", records.len()));
    out.push_str(&format!("  \"total_matches\": {},\n", total));
    out.push_str(&format!("  \"truncated\": {},\n", truncated));
    out.push_str("  \"results\": [");
    if records.is_empty() {
        out.push_str("]\n");
    } else {
        out.push('\n');
        for (index, record) in records.iter().enumerate() {
            out.push_str(&json_record(record, "    "));
            if index + 1 < records.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n");
    }
    out.push_str("}\n");
    out
}

fn render_jsonl(records: &[MatchRecord]) -> String {
    let mut out = String::new();
    for record in records {
        out.push_str(&json_record(record, ""));
        out.push('\n');
    }
    out
}

/// Serialize one record. `indent` is the leading whitespace for the opening
/// brace's *contents*; the opening brace is emitted inline so this composes
/// inside an array (JSON) or on a single line (JSONL, when `indent` is empty).
fn json_record(record: &MatchRecord, indent: &str) -> String {
    let multiline = !indent.is_empty();
    let inner = if multiline {
        format!("{indent}  ")
    } else {
        String::new()
    };
    let nl = if multiline { "\n" } else { "" };
    let sep = if multiline {
        format!(",\n{inner}")
    } else {
        ", ".to_string()
    };

    let mut fields: Vec<String> = Vec::new();
    fields.push(format!("\"kind\": {}", json_string(record.kind.token())));
    fields.push(format!("\"path\": {}", json_string(&record.path_str())));
    fields.push(format!("\"is_dir\": {}", record.is_dir));
    fields.push(format!("\"line\": {}", json_opt_usize(record.line)));
    fields.push(format!("\"column\": {}", json_opt_usize(record.column)));
    fields.push(format!(
        "\"match\": {}",
        match &record.match_line {
            Some(line) => json_string(line),
            None => "null".to_string(),
        }
    ));
    fields.push(format!(
        "\"context_before\": {}",
        json_string_array(&record.context_before)
    ));
    fields.push(format!(
        "\"context_after\": {}",
        json_string_array(&record.context_after)
    ));
    fields.push(format!(
        "\"context_start_line\": {}",
        json_opt_usize(record.context_start_line)
    ));
    fields.push(format!("\"blame\": {}", json_blame(record.blame.as_ref())));

    format!("{indent}{{{nl}{inner}{}{nl}{indent}}}", fields.join(&sep))
}

fn json_blame(blame: Option<&BlameInfo>) -> String {
    match blame {
        None => "null".to_string(),
        Some(info) => format!(
            "{{\"date\": {}, \"timestamp\": {}, \"author\": {}, \"sha\": {}}}",
            json_string(&info.date),
            info.timestamp,
            json_string(&info.author),
            json_string(&info.sha),
        ),
    }
}

fn json_string_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&json_string(item));
    }
    out.push(']');
    out
}

fn json_opt_usize(value: Option<usize>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    }
}

/// JSON-encode a string, including the surrounding quotes. Handles the full
/// set of mandatory escapes plus `\uXXXX` for control characters.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn render_markdown(
    opts: &HeadlessOptions,
    settings: &crate::search::YoinkSettings,
    cwd: &Path,
    records: &[MatchRecord],
    total: usize,
    truncated: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# yoink results: `{}`\n\n", opts.query));
    let mut meta = format!(
        "_mode: {} · sort: {} · case: {} · {} match{} in `{}`",
        settings.search_mode.token(),
        settings.sort.token(),
        case_token(settings.case_sensitive),
        total,
        if total == 1 { "" } else { "es" },
        cwd.to_string_lossy(),
    );
    if truncated {
        meta.push_str(&format!(" (showing first {})", records.len()));
    }
    meta.push_str("_\n\n");
    out.push_str(&meta);

    if records.is_empty() {
        out.push_str("No matches.\n");
        return out;
    }

    for record in records {
        match record.kind {
            Kind::Content => {
                let line = record.line.unwrap_or(0);
                let column = record.column.unwrap_or(0);
                out.push_str(&format!("## `{}:{}:{}`\n", record.path_str(), line, column));
                if let Some(blame) = &record.blame {
                    out.push_str(&format!(
                        "- blame: {} by {}{}\n",
                        blame.date,
                        blame.author,
                        short_sha_suffix(&blame.sha),
                    ));
                }
                out.push('\n');
                out.push_str(&format!("```{}\n", language_for(&record.path)));
                let start = record.context_start_line.unwrap_or(line);
                let mut current = start;
                for ctx in &record.context_before {
                    out.push_str(&format!("{:>5}   {}\n", current, ctx));
                    current += 1;
                }
                if let Some(match_line) = &record.match_line {
                    out.push_str(&format!("{:>5} > {}\n", current, match_line));
                    current += 1;
                }
                for ctx in &record.context_after {
                    out.push_str(&format!("{:>5}   {}\n", current, ctx));
                    current += 1;
                }
                out.push_str("```\n\n");
            }
            Kind::Path => {
                let icon = if record.is_dir { "📁" } else { "📄" };
                out.push_str(&format!("## {} `{}`\n", icon, record.path_str()));
                if let Some(blame) = &record.blame {
                    out.push_str(&format!(
                        "- last touched: {} by {}\n",
                        blame.date, blame.author
                    ));
                }
                out.push('\n');
            }
        }
    }
    out
}

fn short_sha_suffix(sha: &str) -> String {
    if sha.is_empty() {
        String::new()
    } else {
        let short: String = sha.chars().take(8).collect();
        format!(" ({short})")
    }
}

fn render_text(records: &[MatchRecord]) -> String {
    let mut out = String::new();
    for record in records {
        match record.kind {
            Kind::Content => {
                let path = record.path_str();
                let start = record.context_start_line.or(record.line).unwrap_or(0);
                let mut current = start;
                for ctx in &record.context_before {
                    out.push_str(&format!("{}-{}- {}\n", path, current, ctx));
                    current += 1;
                }
                if let Some(match_line) = &record.match_line {
                    out.push_str(&format!("{}:{}: {}\n", path, current, match_line));
                    current += 1;
                }
                for ctx in &record.context_after {
                    out.push_str(&format!("{}-{}- {}\n", path, current, ctx));
                    current += 1;
                }
                out.push_str("--\n");
            }
            Kind::Path => {
                let kind = if record.is_dir { "dir" } else { "file" };
                out.push_str(&format!("{} [{}]\n", record.path_str(), kind));
            }
        }
    }
    out
}

/// Map a file extension to a Markdown code-fence language hint. Best-effort and
/// intentionally small — an unknown extension just yields no hint.
fn language_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "sh" | "bash" | "zsh" => "bash",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        _ => "",
    }
}
