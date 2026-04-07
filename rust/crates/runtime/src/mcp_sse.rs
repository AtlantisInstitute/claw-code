//! SSE (Server-Sent Events) transport for MCP servers.
//!
//! Implements the `McpRemoteTransportDriver` trait. The MCP SSE protocol works as:
//! 1. Client POSTs JSON-RPC requests to the server endpoint
//! 2. Server responds with either plain JSON or SSE event streams
//!
//! This implementation handles both response formats: if the server returns
//! `text/event-stream`, SSE `data:` lines are parsed for the JSON-RPC response.
//! Otherwise, the response is treated as plain JSON.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use reqwest::Client;
use serde_json::Value as JsonValue;

use crate::mcp_stdio::{
    JsonRpcId, JsonRpcResponse, McpInitializeParams, McpInitializeResult,
    McpListResourcesParams, McpListResourcesResult, McpListToolsParams, McpListToolsResult,
    McpReadResourceParams, McpReadResourceResult, McpToolCallParams, McpToolCallResult,
};
use crate::mcp_transport::{
    McpRemoteTransportDriver, build_jsonrpc_request, parse_jsonrpc_response,
};

const DEFAULT_SSE_TIMEOUT_SECS: u64 = 60;

/// MCP transport using SSE (Server-Sent Events) protocol.
///
/// Sends JSON-RPC requests via HTTP POST and handles responses that may
/// arrive as either plain JSON or SSE event streams.
#[derive(Debug)]
pub struct McpSseTransport {
    client: Client,
    endpoint: String,
    headers: BTreeMap<String, String>,
    alive: bool,
}

impl McpSseTransport {
    /// Create a new SSE transport targeting the given endpoint URL.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be built.
    pub fn new(endpoint: &str, headers: BTreeMap<String, String>) -> io::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_SSE_TIMEOUT_SECS))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            headers,
            alive: true,
        })
    }

    /// Send a JSON-RPC request via HTTP POST and parse the response.
    ///
    /// Handles two response content types:
    /// - `text/event-stream`: parses SSE `data:` lines for JSON-RPC payloads
    /// - Everything else: parses as plain JSON
    async fn post_jsonrpc<T: serde::de::DeserializeOwned>(
        &self,
        body: JsonValue,
    ) -> io::Result<JsonRpcResponse<T>> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json");

        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        let response = request
            .json(&body)
            .send()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let text = response
            .text()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if content_type.contains("text/event-stream") {
            parse_sse_response(&text)
        } else {
            let json: JsonValue = serde_json::from_str(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            parse_jsonrpc_response(&json)
        }
    }
}

/// Parse an SSE event stream body to extract the JSON-RPC response.
///
/// Looks for lines prefixed with `data: ` and attempts to parse them as
/// JSON-RPC responses. Returns the first successfully parsed response.
fn parse_sse_response<T: serde::de::DeserializeOwned>(
    text: &str,
) -> io::Result<JsonRpcResponse<T>> {
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if let Ok(json) = serde_json::from_str::<JsonValue>(data) {
                if json.get("jsonrpc").is_some() {
                    return parse_jsonrpc_response(&json);
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no JSON-RPC response found in SSE event stream",
    ))
}

impl McpRemoteTransportDriver for McpSseTransport {
    async fn initialize(
        &mut self,
        id: JsonRpcId,
        params: McpInitializeParams,
    ) -> io::Result<JsonRpcResponse<McpInitializeResult>> {
        let body = build_jsonrpc_request(&id, "initialize", Some(&params))?;
        self.post_jsonrpc(body).await
    }

    async fn list_tools(
        &mut self,
        id: JsonRpcId,
        params: Option<McpListToolsParams>,
    ) -> io::Result<JsonRpcResponse<McpListToolsResult>> {
        let body = build_jsonrpc_request(&id, "tools/list", params.as_ref())?;
        self.post_jsonrpc(body).await
    }

    async fn call_tool(
        &mut self,
        id: JsonRpcId,
        params: McpToolCallParams,
    ) -> io::Result<JsonRpcResponse<McpToolCallResult>> {
        let body = build_jsonrpc_request(&id, "tools/call", Some(&params))?;
        self.post_jsonrpc(body).await
    }

    async fn list_resources(
        &mut self,
        id: JsonRpcId,
        params: Option<McpListResourcesParams>,
    ) -> io::Result<JsonRpcResponse<McpListResourcesResult>> {
        let body = build_jsonrpc_request(&id, "resources/list", params.as_ref())?;
        self.post_jsonrpc(body).await
    }

    async fn read_resource(
        &mut self,
        id: JsonRpcId,
        params: McpReadResourceParams,
    ) -> io::Result<JsonRpcResponse<McpReadResourceResult>> {
        let body = build_jsonrpc_request(&id, "resources/read", Some(&params))?;
        self.post_jsonrpc(body).await
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        self.alive = false;
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive
    }

    fn transport_type(&self) -> &'static str {
        "sse"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_stdio::McpListToolsResult;

    #[test]
    fn sse_transport_creation_succeeds() {
        let transport = McpSseTransport::new(
            "https://example.com/sse",
            BTreeMap::from([("Authorization".to_string(), "Bearer test".to_string())]),
        );
        assert!(transport.is_ok());
    }

    #[test]
    fn sse_transport_type_is_sse() {
        let transport = McpSseTransport::new("https://example.com/sse", BTreeMap::new())
            .expect("should create transport");
        assert_eq!(transport.transport_type(), "sse");
    }

    #[test]
    fn sse_transport_starts_alive() {
        let transport = McpSseTransport::new("https://example.com/sse", BTreeMap::new())
            .expect("should create transport");
        assert!(transport.is_alive());
    }

    #[tokio::test]
    async fn sse_transport_shutdown_marks_not_alive() {
        let mut transport = McpSseTransport::new("https://example.com/sse", BTreeMap::new())
            .expect("should create transport");
        assert!(transport.is_alive());
        transport.shutdown().await.expect("shutdown should succeed");
        assert!(!transport.is_alive());
    }

    #[test]
    fn sse_transport_stores_endpoint_and_headers() {
        let headers = BTreeMap::from([
            ("X-Api-Key".to_string(), "secret".to_string()),
        ]);
        let transport = McpSseTransport::new("https://vendor.example/events", headers.clone())
            .expect("should create transport");
        assert_eq!(transport.endpoint, "https://vendor.example/events");
        assert_eq!(transport.headers, headers);
    }

    #[test]
    fn parse_sse_response_extracts_tools_list() {
        let sse_body = "\
event: message\n\
data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo tool\"}]}}\n\
\n";
        let parsed: JsonRpcResponse<McpListToolsResult> =
            parse_sse_response(sse_body).expect("should parse SSE");
        assert_eq!(parsed.id, JsonRpcId::Number(1));
        let result = parsed.result.expect("should have result");
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "echo");
    }

    #[test]
    fn parse_sse_response_handles_data_without_space() {
        let sse_body = "data:{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n";
        let parsed: JsonRpcResponse<McpListToolsResult> =
            parse_sse_response(sse_body).expect("should parse SSE without space");
        assert_eq!(parsed.id, JsonRpcId::Number(2));
        assert!(parsed.result.unwrap().tools.is_empty());
    }

    #[test]
    fn parse_sse_response_skips_non_jsonrpc_lines() {
        let sse_body = "\
event: ping\n\
data: {\"type\":\"keepalive\"}\n\
\n\
event: message\n\
data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[]}}\n\
\n";
        let parsed: JsonRpcResponse<McpListToolsResult> =
            parse_sse_response(sse_body).expect("should skip non-jsonrpc data");
        assert_eq!(parsed.id, JsonRpcId::Number(3));
    }

    #[test]
    fn parse_sse_response_errors_on_empty_stream() {
        let sse_body = "event: ping\ndata: \n\n";
        let result: io::Result<JsonRpcResponse<McpListToolsResult>> =
            parse_sse_response(sse_body);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("no JSON-RPC response"),
            "should report missing JSON-RPC"
        );
    }

    #[test]
    fn parse_sse_response_extracts_error() {
        let sse_body =
            "data: {\"jsonrpc\":\"2.0\",\"id\":4,\"error\":{\"code\":-32601,\"message\":\"not found\"}}\n";
        let parsed: JsonRpcResponse<JsonValue> =
            parse_sse_response(sse_body).expect("should parse SSE error");
        assert!(parsed.result.is_none());
        let error = parsed.error.expect("should have error");
        assert_eq!(error.code, -32601);
        assert_eq!(error.message, "not found");
    }
}
