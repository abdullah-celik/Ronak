//! A Model Context Protocol server over stdio.
//!
//! An AI client (Claude Code, a local agent, anything that speaks MCP) starts
//! `ronakd mcp` as a child process and exchanges JSON-RPC messages over its
//! stdin and stdout, one per line. Nothing touches the network. This process
//! forwards each tool call to the running daemon over the control socket.
//!
//! Agents can read what the user is doing and *propose* things: a layout, or
//! a card for the Ronak panel written in the surface catalog. A proposal is
//! validated and waits for the user to accept it; an agent can never move a
//! window or draw on the screen by itself. The detail level per client comes
//! from `[mcp]` in the config; the default is task plus app names, never
//! window titles.

use std::path::PathBuf;

use anyhow::Result;
use ronak_core::config::Config;
use ronak_core::model::DetailLevel;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::control;

const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const CONTEXT_SCHEMA: &str = include_str!("../../../schema/context.schema.json");
const SURFACE_SCHEMA: &str = include_str!("../../../schema/surface.schema.json");

const INSTRUCTIONS: &str = "Ronak describes what the user is doing on their desktop right now, \
computed locally on their machine. Use get_current_task for a quick answer and get_current_context \
for the open apps, their roles, what is happening inside the focused app and context such as \
media, calendar and notifications. The user controls how much detail you see; window titles and \
content are only included if they allowed it. You may propose a window layout (propose_layout) or \
a small card for the user's Ronak panel (propose_surface, written in the schema at \
ronak://schema/surface). Proposals only appear; the user decides whether to accept them.";

pub struct McpServer {
    cfg: Config,
    socket: PathBuf,
    detail: DetailLevel,
    client: String,
}

impl McpServer {
    pub fn new(cfg: Config, socket: PathBuf) -> Self {
        let detail = cfg.mcp.default_detail;
        Self { cfg, socket, detail, client: "unknown".into() }
    }

    pub async fn serve_stdio(mut self) -> Result<()> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut stdout = tokio::io::stdout();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.handle(&msg).await,
                Err(e) => {
                    Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }))
                }
            };
            if let Some(r) = response {
                stdout.write_all(format!("{r}\n").as_bytes()).await?;
                stdout.flush().await?;
            }
        }
        Ok(())
    }

    /// Handles one message; notifications (no id) get no response.
    pub async fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?; // notifications such as notifications/initialized
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => Ok(self.call_tool(&params).await),
            "resources/list" => Ok(json!({ "resources": resources() })),
            "resources/read" => self.read_resource(&params).await,
            other => Err((-32601, format!("method not found: {other}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        })
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        let version = if SUPPORTED_VERSIONS.contains(&requested) { requested } else { SUPPORTED_VERSIONS[0] };
        self.client = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("unknown").to_string();
        self.detail = self.cfg.mcp.detail_for(&self.client);
        tracing::info!(client = self.client, detail = ?self.detail, "MCP client connected");
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false, "subscribe": false } },
            "serverInfo": { "name": "ronak", "title": "Ronak desktop context", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })
    }

    async fn daemon(&self, method: &str, params: Value) -> Result<Value, String> {
        control::call(&self.socket, method, params).await.map_err(|e| format!("{e:#}"))
    }

    async fn call_tool(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let detail = json!({ "detail": self.detail });
        let result = match name {
            "get_current_context" => self.daemon("context.get", detail).await,
            "get_current_task" => self.daemon("context.get", json!({ "detail": "task_only" })).await.map(|ctx| {
                json!({ "task": ctx.get("task").cloned().unwrap_or(Value::Null), "activity": ctx.get("activity").cloned().unwrap_or(Value::Null) })
            }),
            "get_layout_suggestion" => self.daemon("suggestion.get", detail).await,
            "get_hardware_profile" => self.daemon("hardware.get", json!({})).await,
            "propose_layout" | "propose_surface" if !self.cfg.mcp.allow_proposals => {
                Err("the user turned proposals off (mcp.allow_proposals = false)".into())
            }
            "propose_layout" => {
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                self.daemon(
                    "propose.layout",
                    json!({
                        "client": self.client,
                        "primary": args.get("primary").cloned().unwrap_or(Value::Null),
                        "references": args.get("references").cloned().unwrap_or(json!([])),
                        "note": args.get("note").cloned().unwrap_or(Value::Null),
                    }),
                )
                .await
            }
            "propose_surface" => {
                let surface = params.pointer("/arguments/surface").cloned().unwrap_or(Value::Null);
                self.daemon("propose.surface", json!({ "client": self.client, "surface": surface })).await
            }
            other => Err(format!("unknown tool '{other}'")),
        };
        match result {
            Ok(value) => json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                "structuredContent": value,
                "isError": false,
            }),
            Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
        }
    }

    async fn read_resource(&self, params: &Value) -> Result<Value, (i64, String)> {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        let (mime, text) = match uri {
            "ronak://context/current" => {
                let ctx =
                    self.daemon("context.get", json!({ "detail": self.detail })).await.map_err(|e| (-32603, e))?;
                ("application/json", serde_json::to_string_pretty(&ctx).unwrap_or_default())
            }
            "ronak://schema/context" => ("application/schema+json", CONTEXT_SCHEMA.to_string()),
            "ronak://schema/surface" => ("application/schema+json", SURFACE_SCHEMA.to_string()),
            _ => return Err((-32002, format!("resource not found: {uri}"))),
        };
        Ok(json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] }))
    }
}

fn read_only(title: &str) -> Value {
    json!({ "title": title, "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false })
}

fn proposal(title: &str) -> Value {
    json!({ "title": title, "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false })
}

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn tools() -> Value {
    json!([
        {
            "name": "get_current_task",
            "title": "Current task",
            "description": "What the user is doing on their computer right now (for example coding, writing, meeting), with a confidence between 0 and 1 and whether they are active or idle.",
            "inputSchema": no_args(),
            "annotations": read_only("Current task"),
        },
        {
            "name": "get_current_context",
            "title": "Current desktop context",
            "description": "The user's current task plus, depending on their privacy setting, the open apps, which one is focused, and the role each window plays (primary, reference, monitor, background). Follows the open Ronak context schema. Window titles appear only if the user granted full detail.",
            "inputSchema": no_args(),
            "annotations": read_only("Current desktop context"),
        },
        {
            "name": "get_layout_suggestion",
            "title": "Suggested layout",
            "description": "The window arrangement Ronak currently suggests for the user's task, if any. Read-only: the user applies it themselves.",
            "inputSchema": no_args(),
            "annotations": read_only("Suggested layout"),
        },
        {
            "name": "get_hardware_profile",
            "title": "AI hardware",
            "description": "Which AI accelerators (NPU, GPU) this computer has, and where Ronak runs its models.",
            "inputSchema": no_args(),
            "annotations": read_only("AI hardware"),
        },
        {
            "name": "propose_layout",
            "title": "Propose a layout",
            "description": "Suggest how to arrange the user's windows: one main app and up to three beside it. Apps are named by app name or app id as get_current_context reports them. The user sees your suggestion with your name and accepts or ignores it; nothing moves without them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "primary": { "type": "string", "description": "The app to give most of the screen, e.g. \"Code\"." },
                    "references": { "type": "array", "items": { "type": "string" }, "maxItems": 3, "description": "Apps to keep beside it." },
                    "note": { "type": "string", "maxLength": 80, "description": "Why, in a few words." }
                },
                "required": ["primary"],
                "additionalProperties": false
            },
            "annotations": proposal("Propose a layout"),
        },
        {
            "name": "propose_surface",
            "title": "Propose a panel card",
            "description": "Show the user a small card in their Ronak panel, built only from the components in ronak://schema/surface (column, row, card, text, badge, meter, button, chips, entry, icon, divider). Buttons may only use the actions the schema lists. The card is validated, labelled with your name and gets Keep and Dismiss buttons; invalid cards are rejected with the reasons.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "surface": {
                        "type": "object",
                        "description": "A surface: { \"title\", \"root\": id, \"components\": [ { \"id\", \"type\", ... } ] }. schema_version, id, kind, composer and created_at are filled in for you."
                    }
                },
                "required": ["surface"],
                "additionalProperties": false
            },
            "annotations": proposal("Propose a panel card"),
        }
    ])
}

fn resources() -> Value {
    json!([
        { "uri": "ronak://context/current", "name": "current-context", "title": "Current desktop context", "mimeType": "application/json",
          "description": "The current context snapshot at this client's detail level." },
        { "uri": "ronak://schema/context", "name": "context-schema", "title": "Ronak context schema", "mimeType": "application/schema+json",
          "description": "JSON Schema describing the context format." },
        { "uri": "ronak://schema/surface", "name": "surface-schema", "title": "Ronak surface schema", "mimeType": "application/schema+json",
          "description": "JSON Schema of the component catalog for propose_surface." }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handshake_and_tool_listing() {
        let mut cfg = Config::default();
        cfg.mcp.clients.insert("trusted-agent".into(), DetailLevel::Full);
        let mut server = McpServer::new(cfg, PathBuf::from("/nonexistent/ronak.sock"));

        let init = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "trusted-agent", "version": "1" } } }))
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(server.detail, DetailLevel::Full);

        assert!(server.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.is_none());

        let list = server.handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await.unwrap();
        let names: Vec<&str> =
            list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "get_current_task",
                "get_current_context",
                "get_layout_suggestion",
                "get_hardware_profile",
                "propose_layout",
                "propose_surface"
            ]
        );

        // With no daemon running, tool calls fail politely instead of crashing.
        let call = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "get_current_task", "arguments": {} } }))
            .await
            .unwrap();
        assert_eq!(call["result"]["isError"], true);
        assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("not running"));

        let schema = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": { "uri": "ronak://schema/context" } }))
            .await
            .unwrap();
        assert!(schema["result"]["contents"][0]["text"].as_str().unwrap().contains("Ronak desktop context snapshot"));
    }

    #[tokio::test]
    async fn unknown_clients_get_the_default_detail() {
        let mut server = McpServer::new(Config::default(), PathBuf::from("/nonexistent"));
        let init = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "1999-01-01", "clientInfo": { "name": "x" } } }))
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], SUPPORTED_VERSIONS[0]);
        assert_eq!(server.detail, DetailLevel::TaskAndApps);
    }
}
