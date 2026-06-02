use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "yoink",
    version,
    about = "Native TUI search — glob or regex, git blame sort, configurable open-bindings"
)]
pub struct Cli {
    #[arg(value_name = "SEARCH")]
    pub query: Option<String>,

    #[command(subcommand)]
    pub internal: Option<InternalCommand>,
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
