//! A minimal Model Context Protocol client over stdio.
//!
//! Ronak already *serves* MCP to AI agents. Here it is the other way around:
//! Ronak starts an MCP server you configured (a GitHub server, a notes
//! server, anything), asks which tools it offers, and can call them when you
//! trigger an action. Newline-delimited JSON-RPC 2.0 on the child's stdin
//! and stdout, as the MCP stdio transport specifies.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{CapsError, Result};

const PROTOCOL_VERSION: &str = "2025-11-25";
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a server told us about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
    pub tools: Vec<String>,
}

pub struct McpClient {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl McpClient {
    /// Starts the server and performs the handshake.
    pub async fn connect(command: &str, args: &[String]) -> Result<(Self, ServerInfo)> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| CapsError::Other(format!("cannot start MCP server {command}: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| CapsError::Other("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| CapsError::Other("no stdout".into()))?;
        let mut client = Self { _child: child, stdin, lines: BufReader::new(stdout).lines(), next_id: 1 };

        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "ronak", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        client.notify("notifications/initialized", json!({})).await?;
        let tools = if init.pointer("/capabilities/tools").is_some() { client.list_tools().await? } else { vec![] };
        let info = ServerInfo {
            name: init.pointer("/serverInfo/name").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            version: init.pointer("/serverInfo/version").and_then(Value::as_str).unwrap_or("").to_string(),
            protocol_version: init.get("protocolVersion").and_then(Value::as_str).unwrap_or("").to_string(),
            tools,
        };
        Ok((client, info))
    }

    async fn send(&mut self, msg: &Value) -> Result<()> {
        let line = format!("{msg}\n");
        self.stdin.write_all(line.as_bytes()).await.map_err(|e| CapsError::Other(e.to_string()))?;
        self.stdin.flush().await.map_err(|e| CapsError::Other(e.to_string()))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    /// Sends a request and waits for the response with the same id, skipping
    /// notifications and requests the server sends in between.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let wait = async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .map_err(|e| CapsError::Other(e.to_string()))?
                    .ok_or_else(|| CapsError::Other("MCP server closed the connection".into()))?;
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if msg.get("id").and_then(Value::as_u64) != Some(id) || msg.get("method").is_some() {
                    continue;
                }
                if let Some(err) = msg.get("error") {
                    let text = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
                    return Err(CapsError::Other(format!("MCP error: {text}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        };
        tokio::time::timeout(TIMEOUT, wait).await.map_err(|_| CapsError::Other(format!("MCP {method} timed out")))?
    }

    pub async fn list_tools(&mut self) -> Result<Vec<String>> {
        let mut names = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
            for t in page.get("tools").and_then(Value::as_array).into_iter().flatten() {
                if let Some(n) = t.get("name").and_then(Value::as_str) {
                    names.push(n.to_string());
                }
            }
            cursor = page.get("nextCursor").and_then(Value::as_str).map(str::to_owned);
            if cursor.is_none() || names.len() > 500 {
                return Ok(names);
            }
        }
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request("tools/call", json!({ "name": name, "arguments": arguments })).await
    }
}
