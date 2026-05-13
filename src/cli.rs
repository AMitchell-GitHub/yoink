use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "yoink", version, about = "TUI regex search using rg + fzf + bat")]
pub struct Cli {
    #[arg(value_name = "SEARCH")]
    pub query: Option<String>,

    #[command(subcommand)]
    pub internal: Option<InternalCommand>,
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    #[command(name = "__search", hide = true)]
    Search {
        #[arg(default_value = "")]
        query: String,
    },
    #[command(name = "__preview", hide = true)]
    Preview {
        path: String,
        #[arg(default_value = "")]
        query: String,
        line: Option<usize>,
    },
    #[command(name = "__toggle_blame", hide = true)]
    ToggleBlame,
    #[command(name = "__prompt", hide = true)]
    Prompt,
    /// Run only when the user toggles INTO blame mode. Takes over the
    /// terminal for the duration of the call to draw an in-place progress
    /// bar, then exits so fzf can redraw and reload from the warm cache.
    #[command(name = "__blame_collect", hide = true)]
    BlameCollect {
        #[arg(default_value = "")]
        query: String,
    },
    #[command(name = "__copy", hide = true)]
    Copy {
        /// "relative" or "filename"
        mode: String,
        path: String,
        #[arg(default_value = "")]
        line: String,
    },
}
