mod cli;
mod indexer;
mod models;
mod parser;
mod search;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    cli::run_cli().await
}
