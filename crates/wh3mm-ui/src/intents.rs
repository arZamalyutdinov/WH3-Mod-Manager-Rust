//! UI intents emitted by any frontend implementation.

/// User intent emitted by a toolkit adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiIntent {
    /// Toggle a mod row.
    ToggleMod {
        /// Toolkit-neutral mod key.
        mod_key: String,
    },
    /// Move a mod before another row. `None` means append.
    MoveMod {
        /// Moved mod key.
        mod_key: String,
        /// Destination row key.
        before_mod_key: Option<String>,
    },
    /// Refresh mod discovery.
    RefreshMods,
    /// Open settings or preferences.
    OpenSettings,
    /// Start the game with the current mod order.
    StartGame,
}
