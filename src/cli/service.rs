//! `install` and `uninstall`: the MCP registration plus the index service.
//!
//! The unit source is packaging/claude-conversation-search-index.service,
//! compiled in. Two layouts:
//!
//! - Packaged: the running binary is /usr/bin/claude-conversation-search and
//!   the package shipped the unit in /usr/lib/systemd/user/. Install only
//!   enables it, after removing a unit an earlier self-managed install wrote
//!   under the user's config dir, which would otherwise override the
//!   packaged one.
//! - Self-managed: any other binary path. Install renders the unit with that
//!   path into $XDG_CONFIG_HOME/systemd/user/.
//!
//! A rendered unit starts with a marker comment. Uninstall deletes a user
//! unit only when it carries the marker, so a unit the user wrote by hand is
//! never removed.
//!
//! The unit keeps ConditionFileIsExecutable on its binary: once the binary is
//! removed, systemd skips the unit and leaves it inactive instead of looping
//! it through restarts into a failed state.
//!
//! When `systemctl --user` cannot reach a user manager, install warns and
//! finishes as standalone: the MCP registration already succeeded and is
//! useful alone. Any systemctl failure after that point is an error carrying
//! systemctl's stderr.
