//! Local-first feature flag system that reads gates from JSON config files,
//! with project-level overrides. Replaces the need for remote feature flag services.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

/// A set of boolean feature gates loaded from JSON config files.
#[derive(Debug, Clone, Default)]
pub struct FeatureFlags {
    gates: BTreeMap<String, bool>,
}

impl FeatureFlags {
    /// Load from a JSON file. Returns empty flags if file doesn't exist or is invalid.
    ///
    /// Expected format: `{ "gates": { "name": true/false, ... } }`
    #[must_use]
    pub fn from_config(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };

        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else {
            return Self::default();
        };

        let Some(gates_obj) = parsed.get("gates").and_then(|g| g.as_object()) else {
            return Self::default();
        };

        let mut gates = BTreeMap::new();
        for (key, value) in gates_obj {
            if let Some(b) = value.as_bool() {
                gates.insert(key.clone(), b);
            }
        }

        Self { gates }
    }

    /// Load base config then overlay project-level overrides.
    ///
    /// Overrides take precedence: if both base and override define the same gate,
    /// the override value wins.
    #[must_use]
    pub fn load_with_overrides(base_path: &Path, overrides_path: Option<&Path>) -> Self {
        let mut flags = Self::from_config(base_path);
        if let Some(overrides) = overrides_path {
            let overrides = Self::from_config(overrides);
            flags.gates.extend(overrides.gates);
        }
        flags
    }

    /// Check a feature gate. Returns `false` for unknown gates.
    #[must_use]
    pub fn check_gate(&self, name: &str) -> bool {
        self.gates.get(name).copied().unwrap_or(false)
    }

    /// Get all gates (for debug/display).
    #[must_use]
    pub fn all_gates(&self) -> &BTreeMap<String, bool> {
        &self.gates
    }

    /// Check if any gates are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gates.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_FLAGS: OnceLock<FeatureFlags> = OnceLock::new();

/// Store a `FeatureFlags` instance for global access. Only the first call wins
/// (subsequent calls are silently ignored, matching `OnceLock` semantics).
pub fn set_global_feature_flags(flags: FeatureFlags) {
    let _ = GLOBAL_FLAGS.set(flags);
}

/// Retrieve the globally-stored feature flags. Returns an empty set if
/// `set_global_feature_flags` was never called.
pub fn global_feature_flags() -> &'static FeatureFlags {
    static EMPTY: OnceLock<FeatureFlags> = OnceLock::new();
    GLOBAL_FLAGS
        .get()
        .unwrap_or_else(|| EMPTY.get_or_init(FeatureFlags::default))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_empty_flags() {
        let flags = FeatureFlags::default();
        assert!(flags.is_empty());
        assert!(flags.all_gates().is_empty());
        assert!(!flags.check_gate("anything"));
    }

    #[test]
    fn test_from_config_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("flags.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"{{ "gates": {{ "dark_mode": true, "beta_ui": false }} }}"#
        )
        .expect("write");

        let flags = FeatureFlags::from_config(&path);
        assert!(!flags.is_empty());
        assert!(flags.check_gate("dark_mode"));
        assert!(!flags.check_gate("beta_ui"));
    }

    #[test]
    fn test_load_with_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");

        let base_path = dir.path().join("base.json");
        let mut f = std::fs::File::create(&base_path).expect("create");
        write!(
            f,
            r#"{{ "gates": {{ "feat_a": true, "feat_b": false }} }}"#
        )
        .expect("write");

        let override_path = dir.path().join("override.json");
        let mut f = std::fs::File::create(&override_path).expect("create");
        write!(f, r#"{{ "gates": {{ "feat_b": true, "feat_c": true }} }}"#).expect("write");

        let flags = FeatureFlags::load_with_overrides(&base_path, Some(&override_path));
        assert!(flags.check_gate("feat_a")); // from base
        assert!(flags.check_gate("feat_b")); // overridden to true
        assert!(flags.check_gate("feat_c")); // from override only
    }

    #[test]
    fn test_missing_file_returns_empty() {
        let flags = FeatureFlags::from_config(Path::new("/nonexistent/path/flags.json"));
        assert!(flags.is_empty());
    }

    #[test]
    fn test_check_gate_default_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("flags.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, r#"{{ "gates": {{ "known": true }} }}"#).expect("write");

        let flags = FeatureFlags::from_config(&path);
        assert!(!flags.check_gate("unknown_gate"));
    }

    #[test]
    fn test_malformed_json_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, "{{{{ not valid json").expect("write");

        let flags = FeatureFlags::from_config(&path);
        assert!(flags.is_empty());
    }
}
