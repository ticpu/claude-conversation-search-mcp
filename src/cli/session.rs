use crate::shared::DisplayOptions;
use crate::shared::session_view::{self, SessionViewOpts};
use anyhow::Result;
use std::path::Path;

pub(crate) fn view_session(index_path: &Path, opts: &SessionViewOpts) -> Result<()> {
    let (entries, source) = session_view::load_session(index_path, &opts.session_id)?;
    if entries.is_empty() {
        anyhow::bail!("no messages found for session {}", opts.session_id);
    }

    let display = DisplayOptions {
        include_thinking: true,
        include_tools: true,
        truncate_length: opts.truncate_length,
    };
    print!(
        "{}",
        session_view::render(&entries, &source, opts, &display)
    );

    if opts.truncate_length > 0 {
        println!("\nUse --full or --truncate 0 for complete content");
    }

    Ok(())
}
