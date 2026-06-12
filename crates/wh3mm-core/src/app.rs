//! Core app state and command handling.

use crate::domain::{GameId, ModIdentity, ModRecord};
use crate::ports::{CoreError, CoreResult};

/// Current UI-independent app state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppState {
    /// Active game context.
    pub active_game: GameId,
    /// Ordered mod list.
    pub mods: Vec<ModRecord>,
}

impl AppState {
    /// Creates an app state from an ordered mod list.
    #[must_use]
    pub fn with_mods(active_game: GameId, mods: Vec<ModRecord>) -> Self {
        Self { active_game, mods }
    }

    /// Applies a core command and returns the resulting event.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`](crate::ports::CoreError) when the command targets
    /// a domain object that is not present in the current state.
    pub fn apply(&mut self, command: CoreCommand) -> CoreResult<CoreEvent> {
        match command {
            CoreCommand::ToggleMod { identity } => self.toggle_mod(&identity),
            CoreCommand::MoveMod {
                identity,
                target_index,
            } => self.move_mod(&identity, target_index),
            CoreCommand::ReplaceMods { mods } => {
                self.mods = mods;
                Ok(CoreEvent::ModsReplaced)
            }
        }
    }

    fn toggle_mod(&mut self, identity: &ModIdentity) -> CoreResult<CoreEvent> {
        let mod_record = self
            .mods
            .iter_mut()
            .find(|mod_record| mod_record.identity.matches(identity))
            .ok_or_else(|| CoreError::mod_not_found(identity.stable_key()))?;

        if mod_record.always_enabled {
            return Ok(CoreEvent::ModEnablementUnchanged {
                mod_key: mod_record.identity.stable_key(),
            });
        }

        mod_record.enabled = !mod_record.enabled;

        Ok(CoreEvent::ModEnablementChanged {
            mod_key: mod_record.identity.stable_key(),
            enabled: mod_record.enabled,
        })
    }

    fn move_mod(&mut self, identity: &ModIdentity, target_index: usize) -> CoreResult<CoreEvent> {
        let from_index = self
            .mods
            .iter()
            .position(|mod_record| mod_record.identity.matches(identity))
            .ok_or_else(|| CoreError::mod_not_found(identity.stable_key()))?;

        let mod_record = self.mods.remove(from_index);
        let to_index = target_index.min(self.mods.len());
        let mod_key = mod_record.identity.stable_key();
        self.mods.insert(to_index, mod_record);

        Ok(CoreEvent::ModMoved {
            mod_key,
            from_index,
            to_index,
        })
    }
}

/// UI-independent commands accepted by the app core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreCommand {
    /// Toggle explicit mod enablement.
    ToggleMod {
        /// Target mod identity.
        identity: ModIdentity,
    },
    /// Move a mod to a target index.
    MoveMod {
        /// Target mod identity.
        identity: ModIdentity,
        /// New zero-based index after removal.
        target_index: usize,
    },
    /// Replace the current ordered mod list.
    ReplaceMods {
        /// Replacement mod list.
        mods: Vec<ModRecord>,
    },
}

/// Observable result of a core command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreEvent {
    /// Mod enablement changed.
    ModEnablementChanged {
        /// Stable mod key.
        mod_key: String,
        /// New explicit enabled state.
        enabled: bool,
    },
    /// Mod was already effectively locked to enabled.
    ModEnablementUnchanged {
        /// Stable mod key.
        mod_key: String,
    },
    /// Mod order changed.
    ModMoved {
        /// Stable mod key.
        mod_key: String,
        /// Previous index.
        from_index: usize,
        /// New index.
        to_index: usize,
    },
    /// Mod list was replaced.
    ModsReplaced,
}

#[cfg(test)]
mod tests {
    use super::{AppState, CoreCommand, CoreEvent};
    use crate::domain::{GameId, ModIdentity, ModRecord};

    fn mod_record(path: &str, workshop_id: Option<&str>, name: &str, enabled: bool) -> ModRecord {
        ModRecord {
            identity: ModIdentity::new(path, workshop_id, name),
            display_name: name.to_string(),
            enabled,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn toggle_uses_path_before_workshop_id() {
        let mut state = AppState::with_mods(
            GameId::Warhammer3,
            vec![
                mod_record("a.pack", Some("123"), "A", false),
                mod_record("b.pack", Some("123"), "B", false),
            ],
        );

        let event = state
            .apply(CoreCommand::ToggleMod {
                identity: ModIdentity::new("b.pack", Some("123"), "B"),
            })
            .unwrap();

        assert_eq!(
            event,
            CoreEvent::ModEnablementChanged {
                mod_key: "path:b.pack".to_string(),
                enabled: true,
            }
        );
        assert!(!state.mods[0].enabled);
        assert!(state.mods[1].enabled);
    }

    #[test]
    fn move_mod_clamps_target_index() {
        let mut state = AppState::with_mods(
            GameId::Warhammer3,
            vec![
                mod_record("a.pack", None, "A", false),
                mod_record("b.pack", None, "B", false),
            ],
        );

        let event = state
            .apply(CoreCommand::MoveMod {
                identity: ModIdentity::new("a.pack", Option::<String>::None, "A"),
                target_index: 99,
            })
            .unwrap();

        assert_eq!(
            event,
            CoreEvent::ModMoved {
                mod_key: "path:a.pack".to_string(),
                from_index: 0,
                to_index: 1,
            }
        );
        assert_eq!(state.mods[1].display_name, "A");
    }
}
