//! Core domain types shared by all UI frontends.

/// Tag prefix used for source pack paths contained inside a generated merged pack.
pub const MERGED_SOURCE_PATH_TAG_PREFIX: &str = "merged-source-path:";

/// Supported game identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameId {
    /// Total War: Warhammer III.
    Warhammer3,
}

/// Stable identity for a mod.
///
/// File path is preferred because workshop IDs can be missing or collide for
/// local/generated packs. Name fallback exists only for legacy data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModIdentity {
    /// Absolute or game-data-relative pack path.
    pub path: String,
    /// Steam Workshop ID when available.
    pub workshop_id: Option<String>,
    /// Pack or display name fallback.
    pub name: String,
}

impl ModIdentity {
    /// Creates a new mod identity.
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        workshop_id: Option<impl Into<String>>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            workshop_id: workshop_id.map(Into::into),
            name: name.into(),
        }
    }

    /// Returns the stable key preferred by UI adapters.
    #[must_use]
    pub fn stable_key(&self) -> String {
        if !self.path.is_empty() {
            return format!("path:{}", self.path);
        }

        if let Some(workshop_id) = &self.workshop_id {
            if !workshop_id.is_empty() {
                return format!("workshop:{workshop_id}");
            }
        }

        format!("name:{}", self.name)
    }

    /// Returns true when this identity should match a command target.
    #[must_use]
    pub fn matches(&self, target: &Self) -> bool {
        if !self.path.is_empty() && !target.path.is_empty() {
            return self.path == target.path;
        }

        match (&self.workshop_id, &target.workshop_id) {
            (Some(left), Some(right)) if !left.is_empty() && !right.is_empty() => left == right,
            _ => !self.name.is_empty() && self.name == target.name,
        }
    }
}

/// Mod data required by core ordering and enablement flows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModRecord {
    /// Stable mod identity.
    pub identity: ModIdentity,
    /// User-facing mod name.
    pub display_name: String,
    /// Whether the user explicitly enabled this mod.
    pub enabled: bool,
    /// Whether the mod is forced on by app/game rules.
    pub always_enabled: bool,
    /// Whether the mod should be hidden from the normal mod-list view.
    pub hidden: bool,
    /// User-defined categories assigned to the mod.
    pub categories: Vec<String>,
    /// User or app tags.
    pub tags: Vec<String>,
}

impl ModRecord {
    /// Returns true when the mod should be treated as active.
    #[must_use]
    pub fn effectively_enabled(&self) -> bool {
        self.enabled || self.always_enabled
    }

    /// Returns source pack paths declared by merged-pack metadata.
    pub fn merged_source_paths(&self) -> impl Iterator<Item = &str> {
        self.tags
            .iter()
            .filter_map(|tag| tag.strip_prefix(MERGED_SOURCE_PATH_TAG_PREFIX))
            .filter(|path| !path.trim().is_empty())
    }
}

/// Builds a namespaced tag for one source pack path inside merged-pack metadata.
#[must_use]
pub fn merged_source_path_tag(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(format!("{MERGED_SOURCE_PATH_TAG_PREFIX}{path}"))
    }
}

impl GameId {
    /// Returns the pack-file magic expected for this game.
    #[must_use]
    pub fn pack_magic(self) -> &'static [u8; 4] {
        match self {
            Self::Warhammer3 => b"PFH5",
        }
    }

    /// Returns whether this game's pack index includes a compression flag.
    #[must_use]
    pub fn supports_pack_compression(self) -> bool {
        match self {
            Self::Warhammer3 => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ModIdentity, ModRecord, merged_source_path_tag};

    #[test]
    fn identity_prefers_path_match() {
        let left = ModIdentity::new("a.pack", Some("123"), "shared");
        let right = ModIdentity::new("b.pack", Some("123"), "shared");

        assert!(!left.matches(&right));
    }

    #[test]
    fn identity_falls_back_to_workshop_id_when_path_missing() {
        let left = ModIdentity::new("", Some("123"), "left");
        let right = ModIdentity::new("", Some("123"), "right");

        assert!(left.matches(&right));
    }

    #[test]
    fn identity_falls_back_to_name_last() {
        let left = ModIdentity::new("", Option::<String>::None, "local");
        let right = ModIdentity::new("", Option::<String>::None, "local");

        assert!(left.matches(&right));
    }

    #[test]
    fn merged_source_path_tags_round_trip_through_mod_record() {
        let tag = merged_source_path_tag(r"C:\mods\a.pack").unwrap();
        let mod_record = ModRecord {
            identity: ModIdentity::new(r"C:\mods\merged.pack", Option::<String>::None, "merged"),
            display_name: "merged".to_string(),
            enabled: true,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: vec!["local".to_string(), tag],
        };

        assert_eq!(
            mod_record.merged_source_paths().collect::<Vec<_>>(),
            [r"C:\mods\a.pack"]
        );
        assert!(merged_source_path_tag("  ").is_none());
    }
}
