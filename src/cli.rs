use crate::search::{SearchMode, Sort};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "yoink",
    version,
    about = "Native TUI search — glob or regex, git blame sort, configurable open-bindings",
    long_about = "yoink searches file/folder names and file contents together.\n\n\
Run with no headless flag to launch the interactive TUI. Pass --output/-o to \
run a one-shot search and print machine-readable results (JSON, JSONL, \
Markdown, or plain text) — handy for scripts and for feeding results to an \
LLM.\n\n\
Quote the query with your shell so spaces, quotes, and metacharacters reach \
yoink intact (prefer 'single quotes'). For a query that starts with '-', use \
--query/-q or put it after a '--' separator.",
    after_help = HEADLESS_HELP,
    after_long_help = HEADLESS_HELP
)]
pub struct Cli {
    /// Search query. Glob by default; regex with --mode regex. Quote it in
    /// your shell to preserve spaces and special characters.
    #[arg(value_name = "SEARCH")]
    pub query: Option<String>,

    /// Search query as an explicit flag. Takes precedence over the positional
    /// query; use it when the query starts with '-' (hyphen values are allowed
    /// here, e.g. `-q '-C'`).
    #[arg(
        short = 'q',
        long = "query",
        value_name = "SEARCH",
        allow_hyphen_values = true
    )]
    pub query_flag: Option<String>,

    /// Print results to stdout in this format instead of launching the TUI.
    /// This is the headless trigger: with it, yoink never opens the TUI.
    #[arg(short = 'o', long = "output", value_name = "FORMAT")]
    pub output: Option<OutputFormat>,

    /// Override the search mode for this run (config is left untouched).
    #[arg(short = 'm', long = "mode", value_name = "MODE")]
    pub mode: Option<ModeArg>,

    /// Override the result ordering for this run (config is left untouched).
    #[arg(short = 's', long = "sort", value_name = "SORT")]
    pub sort: Option<SortArg>,

    /// Override case sensitivity for this run (config is left untouched).
    #[arg(long = "case", value_name = "CASE")]
    pub case: Option<CaseArg>,

    /// Lines of surrounding code to include on each side of a content match
    /// (headless output only).
    #[arg(short = 'C', long = "context", value_name = "N", default_value_t = 10)]
    pub context: usize,

    /// Cap the number of results emitted (headless output only). Applied after
    /// sorting; the JSON envelope reports whether results were truncated.
    #[arg(long = "max-results", visible_alias = "limit", value_name = "N")]
    pub max_results: Option<usize>,

    /// Include git-blame info (date / author / sha) on every result. Blame is
    /// always included for blame sorts; this forces it on for the others too.
    #[arg(long = "blame")]
    pub blame: bool,

    /// Only emit content matches; skip files and directories that matched by
    /// name alone (headless output only).
    #[arg(long = "content-only")]
    pub content_only: bool,

    /// Search across git branches instead of the working tree. Enumerates refs
    /// newest-first and greps each with `git grep <ref>` — nothing is checked
    /// out. Streams results as they're found; pair with --branch-filter,
    /// --since, --ref, and --max-results.
    #[arg(long = "branches")]
    pub branches: bool,

    /// Restrict the cross-branch search to refs whose name matches this glob
    /// (e.g. 'jb/*'). Matched as a substring, so it also catches remote
    /// branches like origin/jb/*. A commit hash here searches just that commit.
    #[arg(long = "branch-filter", visible_alias = "branch", value_name = "GLOB")]
    pub branch_filter: Option<String>,

    /// Only search branches updated within this window: e.g. 360h, 14d, 3w, 1y
    /// (units h/d/w/y, also mo; bare number = days). Omit or 'all' for no limit.
    #[arg(long = "since", value_name = "SPEC")]
    pub since: Option<String>,

    /// Search a specific ref or commit (short or long hash). Repeatable. Skips
    /// enumeration and the name/timeframe filters. Implies --branches.
    #[arg(long = "ref", value_name = "COMMITISH")]
    pub refs: Vec<String>,

    /// Skip the `git fetch` that cross-branch search runs first (fetch is on by
    /// default so teammates' pushed branches are found).
    #[arg(long = "no-fetch")]
    pub no_fetch: bool,

    /// Cross-branch search: only local branches (refs/heads), not
    /// remote-tracking refs.
    #[arg(long = "local-only")]
    pub local_only: bool,

    #[command(subcommand)]
    pub internal: Option<InternalCommand>,
}

/// Examples block appended to `yoink --help`. Explains the headless mode the
/// `-o/--output` flag unlocks and shows copy-pasteable invocations.
const HEADLESS_HELP: &str = "\
HEADLESS MODE (-o/--output): run one search and print to stdout instead of the
TUI — for scripts, pipes, or feeding an LLM. Each match carries the location plus
-C/--context lines each side (default 10); --blame adds commit date/author/sha.

  Formats: markdown (recommended — most readable and compact), json/jsonl (for
  machine parsing), text (grep-style). Tip: add --max-results 30 (or 100) so a
  broad query doesn't print thousands of lines.

  Quoting: single-quote the query so spaces, \", $, and regex survive (like rg).
  For a query starting with '-', use -q/--query or a '--' separator.

BRANCH SEARCH (--branches): search other git branches without checking any out.
Refs are enumerated newest-first and searched with `git grep <ref>`; hits stream
to stdout (progress goes to stderr) so you can stop as soon as you see the match.
Filter with --branch-filter 'jb/*', --since 30d, or target a commit with --ref.
--max-results stops the search early (not just the output). Fetches remotes first
unless --no-fetch. Shows the matched line only (no -C context in branch mode).

EXAMPLES:
  yoink 'fn main(' -o markdown -m regex --max-results 30   # recommended
  yoink 'TODO' -o markdown -C 5 > todos.md                 # export to a file
  yoink 'parseConfig' -o json -s blame_young               # JSON, newest first
  yoink 'name = \"yoink\"' -o text -m regex                  # literal quotes
  yoink -q '-C' -m regex -o markdown                       # query starting '-'
  yoink '__gtp_signed_out__' --branches --branch-filter 'jb/*' --since 30d
  yoink 'signout' --branches -o jsonl --max-results 1      # stream, stop at 1
  yoink 'signout' --ref 1b7c2113 -o text                   # search one commit
";

/// Output format for headless (`--output`) runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// One JSON object: query metadata plus a `results` array. For machine parsing.
    Json,
    /// One JSON object per line, one per result. Stream-friendly.
    Jsonl,
    /// Heading + fenced, line-numbered excerpt per match. Recommended — most
    /// readable and compact.
    Markdown,
    /// Plain grep-style text with context lines.
    Text,
}

/// Search-mode override mirroring `search::SearchMode`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ModeArg {
    Glob,
    Regex,
}

impl ModeArg {
    pub fn to_mode(self) -> SearchMode {
        match self {
            ModeArg::Glob => SearchMode::Glob,
            ModeArg::Regex => SearchMode::Regex,
        }
    }
}

/// Sort override mirroring `search::Sort`. Value names match the config tokens
/// (`blame_young`, `blame_old`) with hyphenated aliases accepted too.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SortArg {
    Depth,
    #[value(alias = "alpha")]
    Alphabetical,
    #[value(name = "blame_young", alias = "blame-young")]
    BlameYoung,
    #[value(name = "blame_old", alias = "blame-old")]
    BlameOld,
}

impl SortArg {
    pub fn to_sort(self) -> Sort {
        match self {
            SortArg::Depth => Sort::Depth,
            SortArg::Alphabetical => Sort::Alphabetical,
            SortArg::BlameYoung => Sort::BlameYoung,
            SortArg::BlameOld => Sort::BlameOld,
        }
    }
}

/// Case-sensitivity override.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CaseArg {
    Sensitive,
    Insensitive,
}

impl CaseArg {
    pub fn is_sensitive(self) -> bool {
        matches!(self, CaseArg::Sensitive)
    }
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    /// Run a one-shot search and print results to stdout. Useful for
    /// scripting and as a diagnostic aid; the interactive TUI doesn't use
    /// it.
    #[command(name = "__search", hide = true)]
    Search {
        #[arg(default_value = "")]
        query: String,
    },
    /// Copy a path or filename to the clipboard. Used by integration tests
    /// to exercise the clipboard-tool detection logic in a subprocess (so
    /// each test gets its own env). Also handy for shell scripts.
    #[command(name = "__copy", hide = true)]
    Copy {
        /// "relative" (path:line) or "filename" (basename only)
        mode: String,
        path: String,
        #[arg(default_value = "")]
        line: String,
    },
}
