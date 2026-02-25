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
    },
}
