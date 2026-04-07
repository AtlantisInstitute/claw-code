//! IDE integration bridge for connecting to external editors (VS Code, etc.).
//!
//! Provides a global [`IdeBridge`] that tool handlers query to check whether an
//! IDE is connected and to relay requests such as diagnostics or code execution.

use std::sync::{Arc, Mutex, OnceLock};

/// Represents the current connection state of an IDE.
#[derive(Debug, Clone)]
pub enum IdeConnectionState {
    /// No IDE is connected.
    Disconnected,
    /// An IDE is connected.  `editor` identifies the IDE (e.g. "vscode").
    Connected { editor: String },
}

/// Thread-safe bridge between the CLI and a connected IDE.
#[derive(Debug, Clone)]
pub struct IdeBridge {
    state: Arc<Mutex<IdeConnectionState>>,
}

impl IdeBridge {
    /// Create a new bridge in the [`Disconnected`](IdeConnectionState::Disconnected) state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(IdeConnectionState::Disconnected)),
        }
    }

    /// Returns `true` when an IDE is actively connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(
            *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            IdeConnectionState::Connected { .. }
        )
    }

    /// Transition to a connected state with the given editor name.
    pub fn connect(&self, editor: String) {
        let mut guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = IdeConnectionState::Connected { editor };
    }

    /// Retrieve language diagnostics from the connected IDE.
    ///
    /// When no IDE is connected the response carries `status: "not_connected"`.
    pub fn get_diagnostics(&self, path: Option<&str>) -> Result<String, String> {
        if !self.is_connected() {
            return Ok(serde_json::json!({
                "status": "not_connected",
                "message": "No IDE connected. Use --ide flag to enable IDE integration.",
                "diagnostics": []
            })
            .to_string());
        }
        // When connected, would communicate with IDE over stdio/websocket.
        // For now, return a placeholder.
        Ok(serde_json::json!({
            "status": "connected",
            "path": path,
            "diagnostics": []
        })
        .to_string())
    }

    /// Execute code in the connected IDE's runtime (e.g. a Jupyter kernel).
    ///
    /// When no IDE is connected the response carries `status: "not_connected"`.
    pub fn execute_code(&self, code: &str, language: &str) -> Result<String, String> {
        if !self.is_connected() {
            return Ok(serde_json::json!({
                "status": "not_connected",
                "message": "No IDE connected. Use --ide flag to enable IDE integration."
            })
            .to_string());
        }
        Ok(serde_json::json!({
            "status": "connected",
            "language": language,
            "code_length": code.len(),
            "result": "Code execution not yet implemented"
        })
        .to_string())
    }
}

impl Default for IdeBridge {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_IDE_BRIDGE: OnceLock<IdeBridge> = OnceLock::new();

/// Register the process-wide [`IdeBridge`].  Ignored if already set.
pub fn set_global_ide_bridge(bridge: IdeBridge) {
    let _ = GLOBAL_IDE_BRIDGE.set(bridge);
}

/// Returns the process-wide [`IdeBridge`], if one has been registered.
#[must_use]
pub fn global_ide_bridge() -> Option<&'static IdeBridge> {
    GLOBAL_IDE_BRIDGE.get()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_connected_default_false() {
        let bridge = IdeBridge::new();
        assert!(!bridge.is_connected());
    }

    #[test]
    fn test_connect_transitions_state() {
        let bridge = IdeBridge::new();
        assert!(!bridge.is_connected());
        bridge.connect("vscode".to_string());
        assert!(bridge.is_connected());
    }

    #[test]
    fn test_disconnected_diagnostics() {
        let bridge = IdeBridge::new();
        let result = bridge.get_diagnostics(Some("/tmp/test.rs")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "not_connected");
        assert!(parsed["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_disconnected_diagnostics_no_path() {
        let bridge = IdeBridge::new();
        let result = bridge.get_diagnostics(None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "not_connected");
    }

    #[test]
    fn test_disconnected_execute() {
        let bridge = IdeBridge::new();
        let result = bridge
            .execute_code("print('hello')", "python")
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "not_connected");
    }

    #[test]
    fn test_connected_diagnostics() {
        let bridge = IdeBridge::new();
        bridge.connect("vscode".to_string());
        let result = bridge.get_diagnostics(Some("/tmp/test.rs")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "connected");
        assert_eq!(parsed["path"], "/tmp/test.rs");
    }

    #[test]
    fn test_connected_execute() {
        let bridge = IdeBridge::new();
        bridge.connect("vscode".to_string());
        let result = bridge.execute_code("1+1", "python").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "connected");
        assert_eq!(parsed["language"], "python");
    }
}
