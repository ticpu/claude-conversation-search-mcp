use crate::shared::SortOrder;
use clap::{Subcommand, ValueEnum};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[derive(Subcommand)]
pub enum CliCommands {
    /// Index management
    Index {
        #[command(subcommand)]
        action: Option<IndexAction>,
    },
    /// Search conversations (auto-indexes if needed)
    Search {
        /// Search query
        query: String,
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
        /// Filter by session ID (prefix match)
        #[arg(long)]
        session: Option<String>,
        /// Results limit
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Context lines before and after match (like grep -C)
        #[arg(short = 'C', default_value = "2")]
        context: usize,
        /// Context lines before match (like grep -B)
        #[arg(short = 'B')]
        ctx_before: Option<usize>,
        /// Context lines after match (like grep -A)
        #[arg(short = 'A')]
        ctx_after: Option<usize>,
        /// Exclude projects by name
        #[arg(long)]
        exclude_project: Vec<String>,
        /// Exclude results matching regex patterns
        #[arg(long)]
        exclude_pattern: Vec<String>,
        /// Sort order
        #[arg(long, value_enum, default_value = "relevance")]
        sort: SortArg,
        /// Results after date (YYYY-MM-DD or ISO 8601)
        #[arg(long)]
        after: Option<String>,
        /// Results before date (YYYY-MM-DD or ISO 8601)
        #[arg(long)]
        before: Option<String>,
        /// Include extra content types
        #[arg(long, value_enum)]
        include: Vec<IncludeArg>,
        /// Characters shown per message (0 = full content)
        #[arg(long, default_value = "300")]
        truncate: usize,
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
        /// Center on a message UUID (prefix match)
        #[arg(long)]
        center: Option<String>,
        /// Context messages before and after center (like grep -C)
        #[arg(short = 'C', default_value = "5")]
        context: usize,
        /// Context messages before center (like grep -B)
        #[arg(short = 'B')]
        before: Option<usize>,
        /// Context messages after center (like grep -A)
        #[arg(short = 'A')]
        after: Option<usize>,
        /// Characters shown per message (0 = full content)
        #[arg(long, default_value = "200")]
        truncate: usize,
        /// Skip this many messages before displaying
        #[arg(long, default_value = "0")]
        offset: usize,
        /// Messages shown (0 = all)
        #[arg(long, default_value = "0")]
        limit: usize,
    },
    /// Summarize a session using Claude (runs in jailed empty dir)
    Summary {
        /// Session ID to summarize
        session_id: String,
    },
    /// Cache management
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    /// Run as MCP server
    Mcp,
    /// Register with Claude MCP
    Install {
        /// Use project scope instead of user scope
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
pub enum CacheAction {
    /// Show cache statistics
    Info,
    /// Clear cache and rebuild
    Clear,
}

#[derive(ValueEnum, Clone, Default)]
pub enum SortArg {
    #[default]
    Relevance,
    DateDesc,
    DateAsc,
}

#[derive(ValueEnum, Clone, PartialEq)]
pub enum IncludeArg {
    Thinking,
    Tools,
}

impl From<SortArg> for SortOrder {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Relevance => SortOrder::Relevance,
            SortArg::DateDesc => SortOrder::DateDesc,
            SortArg::DateAsc => SortOrder::DateAsc,
        }
    }
}

#[derive(Subcommand, Default)]
pub enum IndexAction {
    /// Show index status and statistics (default)
    #[default]
    Status,
    /// Force full rebuild of the index
    Rebuild,
    /// Clean up deleted entries from index
    Vacuum,
}

pub fn setup_logging(verbose: u8) {
    let level = match verbose {
        0 => Level::ERROR,
        1 => Level::WARN,
        2 => Level::INFO,
        _ => Level::DEBUG,
    };

    FmtSubscriber::builder()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .init();
}
