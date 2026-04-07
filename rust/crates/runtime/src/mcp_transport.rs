//! Trait abstraction for MCP transports (stdio, SSE, HTTP).
//!
//! Defines the `McpRemoteTransportDriver` trait that non-stdio transports implement,
//! along with common JSON-RPC request/response helpers shared across HTTP and SSE.

use std::collections::BTreeMap;
use std::io;

use serde_json::Value as JsonValue;

use crate::mcp_stdio::{
    JsonRpcError, JsonRpcId, JsonRpcResponse, McpInitializeParams, McpInitializeResult,
    McpListResourcesParams, McpListResourcesResult, McpListToolsParams, McpListToolsResult,
    McpReadResourceParams, McpReadResourceResult, McpToolCallParams, McpToolCallResult,
};

/// Trait abstracting a non-stdio MCP transport (SSE, HTTP).
///
/// All methods are async and fallible. Implementors must handle their own
/// connection lifecycle (reconnect, keepalive, etc.).
pub trait McpRemoteTransportDriver: Send + std::fmt::Debug {
    /// Send a JSON-RPC initialize request.
    fn initialize(
        &mut self,
        id: JsonRpcId,
        params: McpInitializeParams,
    ) -> impl std::future::Future<Output = io::Result<JsonRpcResponse<McpInitializeResult>>> + Send;

    /// Send a JSON-RPC tools/list request.
    fn list_tools(
        &mut self,
        id: JsonRpcId,
        params: Option<McpListToolsParams>,
    ) -> impl std::future::Future<Output = io::Result<JsonRpcResponse<McpListToolsResult>>> + Send;

    /// Send a JSON-RPC tools/call request.
    fn call_tool(
        &mut self,
        id: JsonRpcId,
        params: McpToolCallParams,
    ) -> impl std::future::Future<Output = io::Result<JsonRpcResponse<McpToolCallResult>>> + Send;

    /// Send a JSON-RPC resources/list request.
    fn list_resources(
        &mut self,
        id: JsonRpcId,
        params: Option<McpListResourcesParams>,
    ) -> impl std::future::Future<Output = io::Result<JsonRpcResponse<McpListResourcesResult>>> + Send;

    /// Send a JSON-RPC resources/read request.
    fn read_resource(
        &mut self,
        id: JsonRpcId,
        params: McpReadResourceParams,
    ) -> impl std::future::Future<Output = io::Result<JsonRpcResponse<McpReadResourceResult>>> + Send;

    /// Shut down the transport.
    fn shutdown(&mut self) -> impl std::future::Future<Output = io::Result<()>> + Send;

    /// Check if transport is still alive.
    fn is_alive(&self) -> bool;

    /// Transport type name for logging/diagnostics.
    fn transport_type(&self) -> &'static str;
}

/// Build a JSON-RPC 2.0 request body as `serde_json::Value`.
pub(crate) fn build_jsonrpc_request<T: serde::Serialize>(
    id: &JsonRpcId,
    method: &str,
    params: Option<&T>,
) -> io::Result<JsonValue> {
    let id_value = match id {
        JsonRpcId::Number(n) => JsonValue::Number((*n).into()),
        JsonRpcId::String(s) => JsonValue::String(s.clone()),
        JsonRpcId::Null => JsonValue::Null,
    };

    let params_value = match params {
        Some(p) => serde_json::to_value(p)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        None => JsonValue::Null,
    };

    Ok(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_value,
        "method": method,
        "params": params_value,
    }))
}

/// Parse a JSON-RPC 2.0 response from raw JSON into the typed response envelope.
pub(crate) fn parse_jsonrpc_response<T: serde::de::DeserializeOwned>(
    json: &JsonValue,
) -> io::Result<JsonRpcResponse<T>> {
    // Validate jsonrpc version
    let version = json
        .get("jsonrpc")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if version != "2.0" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported jsonrpc version: {version}"),
        ));
    }

    // Parse id
    let id = match json.get("id") {
        Some(JsonValue::Number(n)) => {
            JsonRpcId::Number(n.as_u64().unwrap_or(0))
        }
        Some(JsonValue::String(s)) => JsonRpcId::String(s.clone()),
        _ => JsonRpcId::Null,
    };

    // Parse error
    let error = json.get("error").map(|e| {
        let code = e.get("code").and_then(JsonValue::as_i64).unwrap_or(-1);
        let message = e
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string();
        let data = e.get("data").cloned();
        JsonRpcError {
            code,
            message,
            data,
        }
    });

    // Parse result
    let result = json.get("result").and_then(|r| {
        serde_json::from_value::<T>(r.clone()).ok()
    });

    Ok(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result,
        error,
    })
}

/// Convenience: build headers `BTreeMap` into reqwest-compatible `(String, String)` pairs.
#[allow(dead_code)] // available for future transports that need header conversion
pub(crate) fn headers_from_map(map: &BTreeMap<String, String>) -> Vec<(String, String)> {
    map.iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_jsonrpc_request_with_params() {
        let id = JsonRpcId::Number(42);
        let params = serde_json::json!({"cursor": null});
        let body = build_jsonrpc_request(&id, "tools/list", Some(&params)).unwrap();

        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 42);
        assert_eq!(body["method"], "tools/list");
        assert!(body["params"].is_object());
    }

    #[test]
    fn build_jsonrpc_request_without_params() {
        let id = JsonRpcId::String("req-1".to_string());
        let body = build_jsonrpc_request::<JsonValue>(&id, "initialize", None).unwrap();

        assert_eq!(body["id"], "req-1");
        assert_eq!(body["method"], "initialize");
        assert!(body["params"].is_null());
    }

    #[test]
    fn parse_jsonrpc_response_success() {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [],
                "nextCursor": null
            }
        });
        let parsed: JsonRpcResponse<McpListToolsResult> =
            parse_jsonrpc_response(&raw).unwrap();

        assert_eq!(parsed.id, JsonRpcId::Number(1));
        assert!(parsed.error.is_none());
        let result = parsed.result.unwrap();
        assert!(result.tools.is_empty());
    }

    #[test]
    fn parse_jsonrpc_response_error() {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32601,
                "message": "method not found"
            }
        });
        let parsed: JsonRpcResponse<JsonValue> = parse_jsonrpc_response(&raw).unwrap();

        assert!(parsed.result.is_none());
        let error = parsed.error.unwrap();
        assert_eq!(error.code, -32601);
        assert_eq!(error.message, "method not found");
    }

    #[test]
    fn parse_jsonrpc_response_rejects_bad_version() {
        let raw = serde_json::json!({
            "jsonrpc": "1.0",
            "id": 1,
            "result": null
        });
        let result: io::Result<JsonRpcResponse<JsonValue>> = parse_jsonrpc_response(&raw);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported jsonrpc version"));
    }

    #[test]
    fn headers_from_map_converts_correctly() {
        let mut map = BTreeMap::new();
        map.insert("Authorization".to_string(), "Bearer tok".to_string());
        map.insert("X-Custom".to_string(), "value".to_string());
        let headers = headers_from_map(&map);
        assert_eq!(headers.len(), 2);
        assert!(headers.contains(&("Authorization".to_string(), "Bearer tok".to_string())));
    }
}
