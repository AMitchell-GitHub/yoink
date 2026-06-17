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

    #[command(subcommand)]
    pub internal: Option<InternalCommand>,
}

/// Examples block appended to `yoink --help`. Explains the headless mode the
/// `-o/--output` flag unlocks and shows copy-pasteable invocations.
const HEADLESS_HELP: &str = "\
HEADLESS MODE (-o/--output):
  Without -o, yoink launches the interactive TUI. With -o, it runs one search
  and prints the results to stdout — for scripts, pipes, or feeding an LLM.
  Every content match includes the file path, line, column, the matched line,
  and -C/--context lines of code on each side (default 10). Add --blame (or use
  a blame sort) to attach each match's commit date, author, and sha.

FORMATS (-o <FORMAT>):
  json      One object: metadata + a `results` array. The default for tooling.
  jsonl     One result object per line, no envelope. Stream-friendly.
  markdown  A heading per match with a fenced, line-numbered excerpt. Paste-ready.
  text      grep-style `path:line: match` with `path-line-` context lines.

QUOTING THE QUERY:
  There is no special delimiter — let your shell carry the query through, like
  rg or grep. Single-quote it so spaces, \", $, and regex metacharacters survive.
  For a query that starts with '-', use -q/--query or a '--' separator.

EXAMPLES:
  yoink 'fn main(' -o json -m regex            # regex search, JSON output
  yoink ejectReasons -o json                   # glob (default), JSON output
  yoink 'TODO' -o markdown -C 5 > todos.md     # export 5 lines of context
  yoink 'parseConfig' -o jsonl -s blame_young  # newest-blame first, one/line
  yoink 'name = \"yoink\"' -o text -m regex      # literal double-quotes
  yoink -q '-C' -m regex -o json               # query that starts with '-'
  yoink 'panic' -o json --max-results 20       # cap to the first 20 results
  yoink 'Handler' -o json --content-only       # skip name-only file/dir hits
";

/// Output format for headless (`--output`) runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// One JSON object: query metadata plus a `results` array. Default for tooling.
    Json,
    /// One JSON object per line, one per result. Stream-friendly.
    Jsonl,
    /// Markdown with a fenced, line-numbered excerpt per match. Paste-ready.
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
