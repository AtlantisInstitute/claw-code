//! Category-filtered debug logging system.
//!
//! Provides `-d/--debug [filter]` support with per-category filtering
//! and optional file output via `--debug-file <path>`.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Debug output categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DebugCategory {
    Api,
    Hooks,
    Tools,
    Mcp,
    Config,
    Permissions,
    Session,
    All,
}

impl DebugCategory {
    /// Parse a single category name (case-insensitive).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "api" => Ok(Self::Api),
            "hooks" => Ok(Self::Hooks),
            "tools" => Ok(Self::Tools),
            "mcp" => Ok(Self::Mcp),
            "config" => Ok(Self::Config),
            "permissions" => Ok(Self::Permissions),
            "session" => Ok(Self::Session),
            "all" => Ok(Self::All),
            other => Err(format!(
                "unknown debug category: {other} (expected: api,hooks,tools,mcp,config,permissions,session,all)"
            )),
        }
    }
}

/// Configuration for the debug logging system.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugConfig {
    pub enabled: bool,
    pub categories: BTreeSet<DebugCategory>,
}

impl DebugConfig {
    /// Create a disabled configuration (no debug output).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            categories: BTreeSet::new(),
        }
    }

    /// Parse a comma-separated filter string. `None` means all categories.
    pub fn from_filter(filter: Option<&str>) -> Result<Self, String> {
        match filter {
            None => {
                let mut cats = BTreeSet::new();
                cats.insert(DebugCategory::All);
                Ok(Self {
                    enabled: true,
                    categories: cats,
                })
            }
            Some(f) => {
                let mut cats = BTreeSet::new();
                for part in f.split(',') {
                    let part = part.trim();
                    if !part.is_empty() {
                        cats.insert(DebugCategory::parse(part)?);
                    }
                }
                if cats.is_empty() {
                    return Err("empty debug filter".to_string());
                }
                Ok(Self {
                    enabled: true,
                    categories: cats,
                })
            }
        }
    }

    /// Check whether a given category should be logged.
    pub fn should_log(&self, category: DebugCategory) -> bool {
        self.enabled
            && (self.categories.contains(&DebugCategory::All)
                || self.categories.contains(&category))
    }
}

/// Logger that writes debug output to stderr and optionally a file.
pub struct DebugLogger {
    config: DebugConfig,
    file_writer: Option<Mutex<BufWriter<File>>>,
}

impl DebugLogger {
    /// Create a new logger. If `output_file` is provided, log lines are also
    /// written to that file.
    pub fn new(config: DebugConfig, output_file: Option<PathBuf>) -> std::io::Result<Self> {
        let file_writer = if let Some(path) = output_file {
            Some(Mutex::new(BufWriter::new(File::create(path)?)))
        } else {
            None
        };
        Ok(Self {
            config,
            file_writer,
        })
    }

    /// Log a message under the given category (if that category is active).
    pub fn log(&self, category: DebugCategory, message: &str) {
        if !self.config.should_log(category) {
            return;
        }

        let line = format!("[DEBUG:{category:?}] {message}");

        // Write to stderr.
        eprintln!("{line}");

        // Write to file if configured.
        if let Some(ref writer) = self.file_writer {
            if let Ok(mut w) = writer.lock() {
                let _ = writeln!(w, "{line}");
                let _ = w.flush();
            }
        }
    }

    /// Access the underlying configuration.
    pub fn config(&self) -> &DebugConfig {
        &self.config
    }
}

// ── Global logger ───────────────────────────────────────────────────────

static GLOBAL_DEBUG_LOGGER: OnceLock<DebugLogger> = OnceLock::new();

/// Install the global debug logger. Subsequent calls are no-ops.
pub fn set_global_debug_logger(logger: DebugLogger) {
    let _ = GLOBAL_DEBUG_LOGGER.set(logger);
}

/// Log a message through the global logger (if one has been installed and the
/// category is active).
pub fn debug_log(category: DebugCategory, message: &str) {
    if let Some(logger) = GLOBAL_DEBUG_LOGGER.get() {
        logger.log(category, message);
    }
}

/// Returns `true` when the global debug logger is installed and enabled.
pub fn is_debug_enabled() -> bool {
    GLOBAL_DEBUG_LOGGER
        .get()
        .map_or(false, |l| l.config().enabled)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_config_from_filter_all() {
        let cfg = DebugConfig::from_filter(None).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.categories.contains(&DebugCategory::All));
        // All category means every specific category should pass.
        assert!(cfg.should_log(DebugCategory::Api));
        assert!(cfg.should_log(DebugCategory::Hooks));
        assert!(cfg.should_log(DebugCategory::Tools));
        assert!(cfg.should_log(DebugCategory::Mcp));
        assert!(cfg.should_log(DebugCategory::Config));
        assert!(cfg.should_log(DebugCategory::Permissions));
        assert!(cfg.should_log(DebugCategory::Session));
    }

    #[test]
    fn test_debug_config_from_filter_specific() {
        let cfg = DebugConfig::from_filter(Some("api,tools")).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.categories.contains(&DebugCategory::Api));
        assert!(cfg.categories.contains(&DebugCategory::Tools));
        assert!(!cfg.categories.contains(&DebugCategory::Hooks));
        assert!(!cfg.categories.contains(&DebugCategory::All));
    }

    #[test]
    fn test_debug_config_should_log() {
        // Specific categories only.
        let cfg = DebugConfig::from_filter(Some("mcp,session")).unwrap();
        assert!(cfg.should_log(DebugCategory::Mcp));
        assert!(cfg.should_log(DebugCategory::Session));
        assert!(!cfg.should_log(DebugCategory::Api));
        assert!(!cfg.should_log(DebugCategory::Tools));

        // All category.
        let cfg_all = DebugConfig::from_filter(None).unwrap();
        assert!(cfg_all.should_log(DebugCategory::Api));
        assert!(cfg_all.should_log(DebugCategory::Tools));
    }

    #[test]
    fn test_debug_category_parse() {
        // Valid categories.
        assert_eq!(DebugCategory::parse("api").unwrap(), DebugCategory::Api);
        assert_eq!(DebugCategory::parse("hooks").unwrap(), DebugCategory::Hooks);
        assert_eq!(DebugCategory::parse("tools").unwrap(), DebugCategory::Tools);
        assert_eq!(DebugCategory::parse("mcp").unwrap(), DebugCategory::Mcp);
        assert_eq!(
            DebugCategory::parse("config").unwrap(),
            DebugCategory::Config
        );
        assert_eq!(
            DebugCategory::parse("permissions").unwrap(),
            DebugCategory::Permissions
        );
        assert_eq!(
            DebugCategory::parse("session").unwrap(),
            DebugCategory::Session
        );
        assert_eq!(DebugCategory::parse("all").unwrap(), DebugCategory::All);

        // Case insensitive.
        assert_eq!(DebugCategory::parse("API").unwrap(), DebugCategory::Api);
        assert_eq!(DebugCategory::parse("MCP").unwrap(), DebugCategory::Mcp);

        // Invalid.
        assert!(DebugCategory::parse("bogus").is_err());
        assert!(DebugCategory::parse("").is_err());
    }

    #[test]
    fn test_debug_config_disabled() {
        let cfg = DebugConfig::disabled();
        assert!(!cfg.enabled);
        assert!(!cfg.should_log(DebugCategory::Api));
        assert!(!cfg.should_log(DebugCategory::All));
    }

    #[test]
    fn test_debug_config_empty_filter_error() {
        let result = DebugConfig::from_filter(Some(""));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty debug filter"));
    }

    #[test]
    fn test_debug_config_from_filter_with_whitespace() {
        let cfg = DebugConfig::from_filter(Some(" api , hooks ")).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.categories.contains(&DebugCategory::Api));
        assert!(cfg.categories.contains(&DebugCategory::Hooks));
    }

    #[test]
    fn test_debug_config_invalid_category_in_filter() {
        let result = DebugConfig::from_filter(Some("api,bogus"));
        assert!(result.is_err());
    }

    #[test]
    fn test_debug_logger_respects_category_filter() {
        let cfg = DebugConfig::from_filter(Some("api")).unwrap();
        let logger = DebugLogger::new(cfg, None).unwrap();
        // Should not panic; just verifying the plumbing works.
        logger.log(DebugCategory::Api, "should appear");
        logger.log(DebugCategory::Hooks, "should be filtered out");
    }
}
