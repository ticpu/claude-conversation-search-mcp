mod cli;
mod mcp;
mod shared;

use anyhow::Result;
use clap::{CommandFactory, Parser};

#[derive(Parser)]
#[command(name = "claude-conversation-search")]
#[command(version)]
#[command(about = "Search Claude Code conversations and run MCP server")]
struct Cli {
    /// Verbosity level (-v for WARN, -vv for INFO, -vvv for DEBUG)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<cli::CliCommands>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let panic_message = format!("{panic_info}");
        if panic_message.contains("Broken pipe") {
            // Silently exit when pipe is broken (e.g., piped to head, less, etc.)
            std::process::exit(0);
        }
        default_panic(panic_info);
    }));

    let args = Cli::parse();

    match args.command {
        Some(cli::CliCommands::Completions { shell }) => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "claude-conversation-search",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Some(cli::CliCommands::Mcp) | None => {
            // Default to MCP server mode when no subcommand provided
            shared::config::init_config()?;
            mcp::run_mcp_server()
                .await
                .map_err(explain_index_error)
        }
        Some(command) => {
            shared::config::init_config()?;
            cli::run_cli(args.verbose, command).map_err(explain_index_error)
        }
    }
}

/// A config that cannot name the cache dir already failed before any index was opened.
fn explain_index_error(err: anyhow::Error) -> anyhow::Error {
    match shared::get_cache_dir() {
        Ok(index_path) => shared::explain_index_error(err, &index_path),
        Err(_) => err,
    }
}
