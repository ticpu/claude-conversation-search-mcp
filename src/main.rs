use claude_conversation_search::{cli, mcp};

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "claude-conversation-search")]
#[command(about = "Search Claude Code conversations and run MCP server")]
struct Cli {
    /// Verbosity level (-v for WARN, -vv for INFO, -vvv for DEBUG)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Build/update search index
    Index {
        /// Force full rebuild
        #[arg(long)]
        rebuild: bool,
    },
    /// Search conversations (auto-indexes if needed)
    Search {
        /// Search query
        query: String,
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
        /// Results limit
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Show technology topics and their usage across conversations
    Topics {
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
        /// Results limit
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show detailed cache and conversation statistics
    Stats {
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
    },
    /// View specific session conversations
    Session {
        /// Session ID to view
        session_id: String,
        /// Show full content (not just snippets)
        #[arg(long)]
        full: bool,
    },
    /// Cache management
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Run as MCP server
    Mcp,
}

#[derive(Subcommand)]
enum CacheAction {
    /// Show cache statistics
    Info,
    /// Clear cache and rebuild
    Clear,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up panic hook to handle broken pipe errors gracefully
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let panic_message = format!("{panic_info}");
        if panic_message.contains("Broken pipe") {
            // Silently exit when pipe is broken (e.g., piped to head, less, etc.)
            std::process::exit(0);
        }
        default_panic(panic_info);
    }));

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Mcp) | None => {
            // Default to MCP server mode when no subcommand provided
            mcp::run_mcp_server().await
        }
        Some(command) => {
            // Pass CLI args to the CLI handler
            let cli_args = cli::CliArgs {
                verbose: cli.verbose,
                command: match command {
                    Commands::Index { rebuild } => cli::CliCommands::Index { rebuild },
                    Commands::Search {
                        query,
                        project,
                        limit,
                    } => cli::CliCommands::Search {
                        query,
                        project,
                        limit,
                    },
                    Commands::Topics { project, limit } => {
                        cli::CliCommands::Topics { project, limit }
                    }
                    Commands::Stats { project } => cli::CliCommands::Stats { project },
                    Commands::Session { session_id, full } => {
                        cli::CliCommands::Session { session_id, full }
                    }
                    Commands::Cache { action } => cli::CliCommands::Cache {
                        action: match action {
                            CacheAction::Info => cli::CacheAction::Info,
                            CacheAction::Clear => cli::CacheAction::Clear,
                        },
                    },
                    Commands::Mcp => unreachable!(), // Already handled above
                },
            };
            cli::run_cli(cli_args).await
        }
    }
}
