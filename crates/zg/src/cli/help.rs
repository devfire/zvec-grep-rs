//! Leaf subcommands with no nested tree: `help` and `completions`.

use clap::Args;

/// `zg help [topic]`.
#[derive(Debug, Args)]
pub struct HelpArgs {
    /// Command or topic.
    pub topic: Option<String>,
}

/// `zg completions <shell>`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to complete for.
    pub shell: clap_complete::Shell,
}
