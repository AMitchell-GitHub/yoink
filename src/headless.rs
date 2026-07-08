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
    blame_for_file_cached, blame_sort_active, file_last_touched, find_repo_root, format_unix_date,
    SESSION_CACHE_ENV,
};
use crate::branches::{
    branches_containing, parse_timeframe, search_branches, BranchEvent, BranchHit,
    BranchSearchOptions,
};
use crate::cli::OutputFormat;
use crate::search::{
    collect_path_matches, collect_rg_grouped, effective_pattern, load_settings, SearchMode, Sort,
};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Options for a headless cross-branch search (`--branches` / `--ref`),
/// assembled from the parsed CLI. `mode`/`case_sensitive` are `None` when the
/// user didn't pass the flag (config value is then used). `format` is `None`
/// when no `-o` was given — the terminal-friendly grep-style text is streamed.
pub struct BranchHeadlessOptions {
    pub query: String,
    pub format: Option<OutputFormat>,
    pub mode: Option<SearchMode>,
    pub case_sensitive: Option<bool>,
    pub filter: Option<String>,
    pub refs: Vec<String>,
    pub since: Option<String>,
    pub no_fetch: bool,
    pub local_only: bool,
    pub max_results: Option<usize>,
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

#[derive(Debug, Clone, Serialize)]
struct BlameInfo {
    date: String,
    timestamp: i64,
    author: String,
    /// Empty for path-kind results (we use `git log -1`, which has no per-line
    /// sha to attach), so it's omitted from JSON rather than shown blank.
    #[serde(skip_serializing_if = "String::is_empty")]
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

/// One result as serialized to JSON / JSONL. Holds borrows into the underlying
/// `MatchRecord` so building it is allocation-free. Optional fields are omitted
/// (rather than emitted as `null`) when absent — e.g. a path-kind result has no
/// `line`, `match`, or `context`.
#[derive(Serialize)]
struct JsonResult<'a> {
    kind: &'static str,
    path: String,
    is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    match_line: Option<&'a str>,
    /// 1-indexed line number of the first `context` line, so a consumer can
    /// reconstruct absolute line numbers without us repeating one per line.
    #[serde(skip_serializing_if = "Option::is_none")]
    context_start_line: Option<usize>,
    /// The matched line together with the lines before and after it, in source
    /// order — a single ready-to-read block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blame: Option<&'a BlameInfo>,
}

impl<'a> JsonResult<'a> {
    fn from_record(record: &'a MatchRecord) -> JsonResult<'a> {
        let mut context: Vec<&str> = Vec::new();
        context.extend(record.context_before.iter().map(String::as_str));
        if let Some(match_line) = record.match_line.as_deref() {
            context.push(match_line);
        }
        context.extend(record.context_after.iter().map(String::as_str));

        JsonResult {
            kind: record.kind.token(),
            path: record.path_str(),
            is_dir: record.is_dir,
            line: record.line,
            column: record.column,
            match_line: record.match_line.as_deref(),
            context_start_line: record.context_start_line,
            context,
            blame: record.blame.as_ref(),
        }
    }
}

#[derive(Serialize)]
struct JsonEnvelope<'a> {
    query: &'a str,
    mode: &'static str,
    sort: &'static str,
    case: &'static str,
    root: String,
    context_lines: usize,
    count: usize,
    total_matches: usize,
    truncated: bool,
    results: Vec<JsonResult<'a>>,
}

fn render_json(
    opts: &HeadlessOptions,
    settings: &crate::search::YoinkSettings,
    cwd: &Path,
    records: &[MatchRecord],
    total: usize,
    truncated: bool,
) -> String {
    let envelope = JsonEnvelope {
        query: &opts.query,
        mode: settings.search_mode.token(),
        sort: settings.sort.token(),
        case: case_token(settings.case_sensitive),
        root: cwd.to_string_lossy().into_owned(),
        context_lines: opts.context,
        count: records.len(),
        total_matches: total,
        truncated,
        results: records.iter().map(JsonResult::from_record).collect(),
    };
    // Pretty-printed so the output is human-readable and pastes into an editor
    // as well-formed JSON. Serialization of these plain structs cannot fail;
    // fall back to an empty object defensively.
    let mut out = serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
    out
}

fn render_jsonl(records: &[MatchRecord]) -> String {
    let mut out = String::new();
    for record in records {
        let result = JsonResult::from_record(record);
        if let Ok(line) = serde_json::to_string(&result) {
            out.push_str(&line);
            out.push('\n');
        }
    }
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

                // Excerpt = before + matched line + after. The fence is sized
                // off the raw lines; the rendered block carries a line-number
                // gutter with the matched line marked by `>`.
                let mut excerpt: Vec<&str> = Vec::new();
                excerpt.extend(record.context_before.iter().map(String::as_str));
                if let Some(match_line) = &record.match_line {
                    excerpt.push(match_line);
                }
                excerpt.extend(record.context_after.iter().map(String::as_str));

                let start = record.context_start_line.unwrap_or(line);
                let last = start + excerpt.len().saturating_sub(1);
                let width = last.to_string().len().max(3);

                let fence = code_fence_for(&excerpt);
                out.push_str(&format!("{}{}\n", fence, language_for(&record.path)));
                let mut current = start;
                for ctx in &record.context_before {
                    out.push_str(&format!("{current:>width$}   {ctx}\n"));
                    current += 1;
                }
                if let Some(match_line) = &record.match_line {
                    out.push_str(&format!("{current:>width$} > {match_line}\n"));
                    current += 1;
                }
                for ctx in &record.context_after {
                    out.push_str(&format!("{current:>width$}   {ctx}\n"));
                    current += 1;
                }
                out.push_str(&format!("{}\n\n", fence));
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

/// Pick a fence that's longer than the longest backtick run in the excerpt, so
/// code containing ``` doesn't prematurely close the block. At least three.
fn code_fence_for(lines: &[&str]) -> String {
    let mut longest_run = 0usize;
    for line in lines {
        let mut run = 0usize;
        for ch in line.chars() {
            if ch == '`' {
                run += 1;
                longest_run = longest_run.max(run);
            } else {
                run = 0;
            }
        }
    }
    "`".repeat(longest_run.max(2) + 1)
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

// ---------------------------------------------------------------------------
// Cross-branch search (--branches / --ref)
// ---------------------------------------------------------------------------

/// A single cross-branch hit, serialized to JSON / JSONL. Borrows the hit so
/// building it is allocation-light. `committed` is the ref's committer date.
#[derive(Serialize)]
struct JsonBranchResult<'a> {
    branch: &'a str,
    path: String,
    line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    committed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    committed_date: Option<String>,
    content: &'a str,
}

impl<'a> JsonBranchResult<'a> {
    fn from_hit(hit: &'a BranchHit) -> JsonBranchResult<'a> {
        JsonBranchResult {
            branch: &hit.reference,
            path: hit.path.to_string_lossy().into_owned(),
            line: hit.line,
            committed: hit.committed_ts,
            committed_date: hit.committed_ts.map(format_unix_date),
            content: &hit.content,
        }
    }
}

#[derive(Serialize)]
struct JsonBranchEnvelope<'a> {
    query: &'a str,
    mode: &'static str,
    case: &'static str,
    root: String,
    refs_searched: usize,
    count: usize,
    truncated: bool,
    results: Vec<JsonBranchResult<'a>>,
}

/// grep-style, greppable one line per hit: `<ref>:<path>:<line>:<content>`.
fn write_branch_text(out: &mut impl Write, hit: &BranchHit) -> io::Result<()> {
    writeln!(
        out,
        "{}:{}:{}:{}",
        hit.reference,
        hit.path.display(),
        hit.line,
        hit.content
    )?;
    out.flush()
}

fn write_branch_jsonl(out: &mut impl Write, hit: &BranchHit) -> io::Result<()> {
    if let Ok(line) = serde_json::to_string(&JsonBranchResult::from_hit(hit)) {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

fn write_branch_markdown(out: &mut impl Write, hit: &BranchHit) -> io::Result<()> {
    writeln!(
        out,
        "## `{}` — {}:{}",
        hit.reference,
        hit.path.display(),
        hit.line
    )?;
    if let Some(ts) = hit.committed_ts {
        writeln!(out, "_{}_", format_unix_date(ts))?;
    }
    let lang = language_for(&hit.path);
    writeln!(out, "```{lang}")?;
    writeln!(out, "{}", hit.content)?;
    writeln!(out, "```\n")?;
    out.flush()
}

/// Run a one-shot cross-branch search. Hits stream to stdout in the requested
/// format (grep-style text by default); progress goes to stderr so stdout stays
/// clean for pipes. A closed downstream pipe (`| head`) ends the run cleanly.
pub fn run_branch_search(opts: BranchHeadlessOptions, cwd: &Path) -> Result<()> {
    if opts.query.trim().is_empty() {
        bail!("a non-empty query is required for --branches (try: yoink 'pattern' --branches)");
    }
    let repo_root = find_repo_root(cwd).ok_or_else(|| {
        anyhow!(
            "not inside a git repository (no .git found from {})",
            cwd.display()
        )
    })?;

    let mut settings = load_settings()?;
    if let Some(mode) = opts.mode {
        settings.search_mode = mode;
    }
    if let Some(case_sensitive) = opts.case_sensitive {
        settings.case_sensitive = case_sensitive;
    }
    let since = parse_timeframe(opts.since.as_deref().unwrap_or(""))?;

    let format = opts.format;
    let mode_token = settings.search_mode.token();
    let case_tok = case_token(settings.case_sensitive);
    let root_display = repo_root.to_string_lossy().into_owned();

    let search_opts = BranchSearchOptions {
        query: opts.query.clone(),
        mode: settings.search_mode,
        case_sensitive: settings.case_sensitive,
        filter: opts.filter.clone(),
        explicit_refs: opts.refs.clone(),
        since,
        include_local: true,
        include_remotes: !opts.local_only,
        fetch: !opts.no_fetch,
        max_results: opts.max_results,
    };

    // Bonus for a bare commit target: note which branches contain it (stderr).
    let hashy = |value: &str| {
        let len = value.len();
        (4..=40).contains(&len) && value.chars().all(|c| c.is_ascii_hexdigit())
    };
    let commit_target = if opts.refs.len() == 1 && hashy(&opts.refs[0]) {
        Some(opts.refs[0].clone())
    } else {
        opts.filter
            .as_deref()
            .filter(|f| hashy(f))
            .map(str::to_string)
    };
    if let Some(commit) = &commit_target {
        let containing = branches_containing(&repo_root, commit);
        if !containing.is_empty() {
            eprintln!(
                "… commit {commit} is contained in: {}",
                containing.join(", ")
            );
        }
    }

    let cancel = AtomicBool::new(false);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut broken_pipe = false;
    let mut collected: Vec<BranchHit> = Vec::new();

    if matches!(format, Some(OutputFormat::Markdown)) {
        let _ = writeln!(out, "# yoink branch results\n");
        let _ = writeln!(
            out,
            "_query `{}` · {mode_token} · {case_tok}_\n",
            opts.query
        );
    }

    let outcome = search_branches(&repo_root, &search_opts, &cancel, |event| match event {
        BranchEvent::Fetching => eprintln!("… fetching remotes (git fetch --all)"),
        BranchEvent::FetchFailed(err) => {
            eprintln!("! fetch failed ({err}); searching refs already present")
        }
        BranchEvent::Enumerated { total } => eprintln!("… searching {total} ref(s), newest first"),
        BranchEvent::RefStarted { index, total, name } => {
            eprintln!("… [{}/{total}] {name}", index + 1)
        }
        BranchEvent::Hit(hit) => {
            if broken_pipe {
                return;
            }
            let written = match format {
                Some(OutputFormat::Jsonl) => write_branch_jsonl(&mut out, &hit),
                Some(OutputFormat::Markdown) => write_branch_markdown(&mut out, &hit),
                Some(OutputFormat::Json) => {
                    collected.push(hit);
                    Ok(())
                }
                Some(OutputFormat::Text) | None => write_branch_text(&mut out, &hit),
            };
            if let Err(err) = written {
                if err.kind() == ErrorKind::BrokenPipe {
                    broken_pipe = true;
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        BranchEvent::Finished {
            searched,
            hits,
            truncated,
        } => {
            if matches!(format, Some(OutputFormat::Json)) && !broken_pipe {
                let envelope = JsonBranchEnvelope {
                    query: &opts.query,
                    mode: mode_token,
                    case: case_tok,
                    root: root_display.clone(),
                    refs_searched: searched,
                    count: collected.len(),
                    truncated,
                    results: collected.iter().map(JsonBranchResult::from_hit).collect(),
                };
                let mut text =
                    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_string());
                text.push('\n');
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            eprintln!(
                "done: {hits} hit(s) across {searched} ref(s){}",
                if truncated {
                    " (stopped early — more may exist)"
                } else {
                    ""
                }
            );
        }
    });

    // A closed downstream pipe is a normal, successful early exit.
    if broken_pipe {
        return Ok(());
    }
    outcome
}
