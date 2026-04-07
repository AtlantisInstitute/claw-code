//! HTTP transport for MCP servers.
//!
//! Implements the `McpRemoteTransportDriver` trait using simple HTTP POST
//! with JSON-RPC request/response over `reqwest`.

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

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// MCP transport that sends JSON-RPC requests as HTTP POST to a remote endpoint.
#[derive(Debug)]
pub struct McpHttpTransport {
    client: Client,
    endpoint: String,
    headers: BTreeMap<String, String>,
    alive: bool,
}

impl McpHttpTransport {
    /// Create a new HTTP transport targeting the given endpoint URL.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be built.
    pub fn new(endpoint: &str, headers: BTreeMap<String, String>) -> io::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            headers,
            alive: true,
        })
    }

    /// Send a JSON-RPC request via HTTP POST and return the parsed response.
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

        let json: JsonValue = response
            .json()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        parse_jsonrpc_response(&json)
    }
}

impl McpRemoteTransportDriver for McpHttpTransport {
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
        "http"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_transport_creation_succeeds() {
        let transport = McpHttpTransport::new(
            "https://example.com/mcp",
            BTreeMap::from([("Authorization".to_string(), "Bearer test".to_string())]),
        );
        assert!(transport.is_ok());
    }

    #[test]
    fn http_transport_type_is_http() {
        let transport = McpHttpTransport::new("https://example.com/mcp", BTreeMap::new())
            .expect("should create transport");
        assert_eq!(transport.transport_type(), "http");
    }

    #[test]
    fn http_transport_starts_alive() {
        let transport = McpHttpTransport::new("https://example.com/mcp", BTreeMap::new())
            .expect("should create transport");
        assert!(transport.is_alive());
    }

    #[tokio::test]
    async fn http_transport_shutdown_marks_not_alive() {
        let mut transport = McpHttpTransport::new("https://example.com/mcp", BTreeMap::new())
            .expect("should create transport");
        assert!(transport.is_alive());
        transport.shutdown().await.expect("shutdown should succeed");
        assert!(!transport.is_alive());
    }

    #[test]
    fn http_transport_stores_endpoint_and_headers() {
        let headers = BTreeMap::from([
            ("X-Api-Key".to_string(), "secret".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ]);
        let transport = McpHttpTransport::new("https://vendor.example/v1/mcp", headers.clone())
            .expect("should create transport");
        assert_eq!(transport.endpoint, "https://vendor.example/v1/mcp");
        assert_eq!(transport.headers, headers);
    }
}
