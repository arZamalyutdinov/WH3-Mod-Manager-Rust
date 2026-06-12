//! Ports for infrastructure adapters.
//!
//! Filesystem, Steam, package, and platform operations should enter core
//! through traits like these instead of through UI toolkit code.

use crate::domain::ModRecord;
use std::io;

/// Core result type.
pub type CoreResult<T> = Result<T, CoreError>;

/// Error type used by the domain core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreError {
    /// Stable machine-readable error kind.
    pub kind: CoreErrorKind,
    /// Human-readable diagnostic.
    pub message: String,
}

impl CoreError {
    /// Creates a not-found error for a missing mod.
    #[must_use]
    pub fn mod_not_found(mod_key: impl Into<String>) -> Self {
        let mod_key = mod_key.into();
        Self {
            kind: CoreErrorKind::NotFound,
            message: format!("mod not found: {mod_key}"),
        }
    }

    /// Creates an invalid-input error.
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            kind: CoreErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    /// Creates a malformed-pack parse error.
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self {
            kind: CoreErrorKind::Parse,
            message: message.into(),
        }
    }

    /// Creates an IO-boundary error.
    #[must_use]
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: CoreErrorKind::Io,
            message: message.into(),
        }
    }

    /// Creates an external adapter error.
    #[must_use]
    pub fn adapter(message: impl Into<String>) -> Self {
        Self {
            kind: CoreErrorKind::Adapter,
            message: message.into(),
        }
    }
}

impl From<io::Error> for CoreError {
    fn from(error: io::Error) -> Self {
        Self::io(error.to_string())
    }
}

/// Stable error categories for UI adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreErrorKind {
    /// Requested item was not found.
    NotFound,
    /// Input data was invalid.
    InvalidInput,
    /// Parsed data was malformed or unsupported.
    Parse,
    /// Filesystem or OS boundary failed.
    Io,
    /// External adapter failed.
    Adapter,
}

/// Adapter boundary for loading mods.
pub trait ModRepository {
    /// Loads the current ordered mod list.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when the adapter cannot read or decode its mod
    /// source.
    fn load_mods(&self) -> CoreResult<Vec<ModRecord>>;
}
