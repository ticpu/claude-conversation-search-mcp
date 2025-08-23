mod cache;
mod cli;
mod indexer;
mod metadata;
mod models;
mod parser;
mod search;

use anyhow::Result;

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

    cli::run_cli().await
}
