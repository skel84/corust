//! Coroot's MCP endpoint (JSON-RPC over streamable HTTP).
//!
//! Some data is only available through MCP: PromQL range queries for regular users,
//! application health summaries, traces. MCP requires an API key.

use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::Client;
use crate::error::{Error, ErrorKind, Result};

const SESSION_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_VERSION: &str = "2025-03-26";

/// An open MCP session. Call [`McpSession::close`] when done; dropping the session
/// without closing it leaves it to expire on the server.
///
/// The typed methods on [`Project`](crate::Project) open and close a session per call;
/// use a session directly to make several calls over one connection or to call tools
/// this crate does not wrap.
pub struct McpSession {
    client: Client,
    session: Option<String>,
    next_id: u64,
}

impl std::fmt::Debug for McpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSession")
            .field("session", &self.session)
            .finish()
    }
}

/// The result of a tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    /// The tool returned JSON.
    Json(Value),
    /// The tool returned text: usually a result over the size budget, truncated.
    Text(String),
}

impl ToolOutput {
    /// The JSON value, with text as a JSON string.
    pub fn into_value(self) -> Value {
        match self {
            ToolOutput::Json(v) => v,
            ToolOutput::Text(t) => Value::String(t),
        }
    }
}

impl McpSession {
    pub(crate) async fn connect(client: Client) -> Result<Self> {
        if !client.credentials().is_api_key() {
            return Err(Error::new(
                ErrorKind::Auth,
                "Coroot's MCP endpoint requires an API key (Coroot UI → user menu → API keys)",
            ));
        }
        let mut m = McpSession {
            client,
            session: None,
            next_id: 1,
        };
        m.rpc(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "coroot-rs", "version": env!("CARGO_PKG_VERSION")},
            }),
        )
        .await
        .map_err(|e| match e.kind() {
            ErrorKind::Unsupported | ErrorKind::NotFound | ErrorKind::Decode => Error::new(
                ErrorKind::Unsupported,
                format!("cannot initialize an MCP session (Coroot 1.14+ is required): {e}"),
            ),
            _ => e,
        })?;
        m.notify("notifications/initialized").await?;
        Ok(m)
    }

    pub(crate) async fn connect_project(client: Client, project: &str) -> Result<Self> {
        let mut m = Self::connect(client).await?;
        m.call_tool("select_project", json!({"project_id": project}))
            .await?;
        Ok(m)
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut rb = self
            .client
            .request(Method::POST, "mcp")?
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(serde_json::to_vec(body)?);
        if let Some(s) = &self.session {
            rb = rb.header(SESSION_HEADER, s);
        }
        self.client.execute(rb).await
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        self.post(&json!({"jsonrpc": "2.0", "method": method}))
            .await?;
        Ok(())
    }

    /// Sends a JSON-RPC request and returns its `result`.
    pub async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let resp = self
            .post(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        if let Some(s) = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(s.to_string());
        }
        let is_sse = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        let text = resp.text().await.map_err(|e| {
            Error::new(
                ErrorKind::Network,
                format!("cannot read the MCP response: {e}"),
            )
        })?;
        let msg = if is_sse {
            find_sse_response(&text, id)?
        } else {
            serde_json::from_str(&text)?
        };
        if let Some(e) = msg.get("error") {
            let m = e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(classify(&format!("MCP error: {m}")));
        }
        msg.get("result")
            .cloned()
            .ok_or_else(|| Error::decode("MCP response without a result"))
    }

    /// Calls a tool. Tool errors become typed errors.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<ToolOutput> {
        let res = self
            .rpc("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        let text = res
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if res.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(classify(&text));
        }
        if text.trim().is_empty() {
            return Ok(ToolOutput::Json(Value::Null));
        }
        Ok(match serde_json::from_str(&text) {
            Ok(v) => {
                self.client.observe(&format!("mcp:{name}"), &v);
                ToolOutput::Json(v)
            }
            Err(_) => ToolOutput::Text(text),
        })
    }

    /// Calls a tool that returns JSON, failing on text results (results over the size budget).
    pub(crate) async fn call_json(&mut self, name: &str, args: Value) -> Result<Value> {
        match self.call_tool(name, args).await? {
            ToolOutput::Json(v) => Ok(v),
            ToolOutput::Text(t) => Err(too_large(&t)),
        }
    }

    /// The tools the server offers, as their JSON descriptors.
    pub async fn list_tools(&mut self) -> Result<Vec<Value>> {
        let res = self.rpc("tools/list", json!({})).await?;
        Ok(res
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Ends the session on the server.
    pub async fn close(mut self) {
        if let Some(s) = self.session.take()
            && let Ok(rb) = self.client.request(Method::DELETE, "mcp")
        {
            let _ = rb.header(SESSION_HEADER, s).send().await;
        }
    }
}

/// Tools answer with text instead of JSON when the result does not fit their size budget.
fn too_large(text: &str) -> Error {
    let text = text.trim();
    let mut msg: String = text.chars().take(500).collect();
    if msg.len() < text.len() {
        msg.push('…');
    }
    Error::new(ErrorKind::InvalidInput, msg)
}

/// A list from an MCP tool, possibly cut to fit the response size budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Listing<T> {
    /// Items before truncation.
    #[serde(default)]
    pub total: u64,
    /// Items in this response.
    #[serde(default)]
    pub returned: u64,
    /// Whether items were left out to keep the response within the size budget.
    #[serde(default, skip_serializing_if = "crate::json::is_false")]
    pub truncated: bool,
    /// How to narrow the query, when truncated.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    /// The items, in the order the tool ranks them.
    #[serde(default, deserialize_with = "crate::json::null_default")]
    pub items: Vec<T>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Listing {
            total: 0,
            returned: 0,
            truncated: false,
            hint: String::new(),
            items: Vec::new(),
        }
    }
}

impl<T: serde::de::DeserializeOwned> Listing<T> {
    /// Accepts a listing, a bare array, or null (no data).
    pub(crate) fn from_value(v: Value) -> Result<Self> {
        Ok(match v {
            Value::Null => Listing::default(),
            Value::Array(items) => {
                let items: Vec<T> = serde_json::from_value(Value::Array(items))?;
                Listing {
                    total: items.len() as u64,
                    returned: items.len() as u64,
                    items,
                    ..Listing::default()
                }
            }
            v => serde_json::from_value(v)?,
        })
    }
}

/// Maps an MCP error message to a typed error.
fn classify(msg: &str) -> Error {
    let m = msg.to_lowercase();
    let kind = if m.contains("forbidden") || m.contains("not allowed") {
        ErrorKind::Forbidden
    } else if m.contains("not found") {
        ErrorKind::NotFound
    } else if m.contains("invalid")
        || m.contains("parse error")
        || m.contains("required")
        || m.contains("must be")
    {
        ErrorKind::InvalidInput
    } else {
        ErrorKind::Server
    };
    Error::new(kind, msg.trim())
}

/// Finds the JSON-RPC response with the given id in a server-sent events stream.
fn find_sse_response(body: &str, id: u64) -> Result<Value> {
    for event in body.split("\n\n") {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n"))
            && v.get("id").and_then(Value::as_u64) == Some(id)
        {
            return Ok(v);
        }
    }
    Err(Error::decode(format!(
        "no response for request {id} in the MCP event stream"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n";
        let v = find_sse_response(body, 7).unwrap();
        assert_eq!(v["result"]["ok"], json!(true));
        assert!(find_sse_response(body, 8).is_err());
    }

    #[test]
    fn classification() {
        assert_eq!(classify("app not found").kind(), ErrorKind::NotFound);
        assert_eq!(
            classify("parse error at 1:3").kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(classify("boom").kind(), ErrorKind::Server);
    }
}
