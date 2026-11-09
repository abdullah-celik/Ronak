//! The control socket: how the CLI and the MCP bridge talk to the running
//! daemon.
//!
//! A Unix domain socket is a file-like endpoint for local inter-process
//! communication. It lives in `$XDG_RUNTIME_DIR/ronak/` (a per-user tmpfs) in
//! a directory only you can enter, so other users cannot connect. The wire
//! format is JSON-RPC 2.0, one JSON object per line. On Windows this will be
//! a named pipe with the same protocol.
//!
//! One method keeps the connection open: `surface.subscribe`. After it, the
//! daemon pushes `surfaces` notifications (JSON-RPC messages without an id)
//! whenever the panel's content changes. That is how the panel app works.

use std::path::Path;

use anyhow::{Context, Result, bail};
use ronak_core::model::DetailLevel;
use ronak_core::surface::UiAction;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::daemon::{ControlMsg, ControlRequest};

/// Maps a JSON-RPC method and params to a daemon request.
pub fn parse_request(method: &str, params: &Value) -> Result<ControlRequest, (i64, String)> {
    let detail = || -> Result<DetailLevel, (i64, String)> {
        match params.get("detail") {
            None | Some(Value::Null) => Ok(DetailLevel::TaskAndApps),
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| (-32602, format!("invalid detail: {e}"))),
        }
    };
    Ok(match method {
        "status" => ControlRequest::Status,
        "context.get" => ControlRequest::Context { detail: detail()? },
        "suggestion.get" => ControlRequest::Suggestion { detail: detail()? },
        "suggestion.apply" => ControlRequest::Apply,
        "suggestion.dismiss" => ControlRequest::Dismiss,
        "undo" => ControlRequest::Undo,
        "observe.set" => ControlRequest::SetObserving(
            params
                .get("observing")
                .and_then(Value::as_bool)
                .ok_or((-32602, "missing boolean 'observing'".to_string()))?,
        ),
        "hardware.get" => ControlRequest::Hardware,
        "intent.correct" => ControlRequest::Correct {
            label: str_param(params, "label")?.ok_or((-32602, "missing string 'label'".to_string()))?,
            activity: str_param(params, "activity")?,
            project: str_param(params, "project")?,
        },
        "surface.action" => ControlRequest::SurfaceAction {
            action: serde_json::from_value::<UiAction>(params.get("action").cloned().unwrap_or(Value::Null))
                .map_err(|e| (-32602, format!("invalid action: {e}")))?,
            text: str_param(params, "text")?,
        },
        "panel.toggle" => ControlRequest::Panel { visible: None, style: None },
        "panel.set" => ControlRequest::Panel {
            visible: match params.get("visible") {
                None | Some(Value::Null) => None,
                Some(Value::Bool(b)) => Some(*b),
                Some(_) => return Err((-32602, "'visible' must be a boolean or null".into())),
            },
            style: match params.get("style") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    Some(serde_json::from_value(v.clone()).map_err(|e| (-32602, format!("invalid style: {e}")))?)
                }
            },
        },
        "propose.layout" => ControlRequest::ProposeLayout {
            client: client_name(params),
            primary: str_param(params, "primary")?.ok_or((-32602, "missing string 'primary'".to_string()))?,
            references: params
                .get("references")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            note: str_param(params, "note")?,
        },
        "propose.surface" => ControlRequest::ProposeSurface {
            client: client_name(params),
            surface: params.get("surface").cloned().ok_or((-32602, "missing object 'surface'".to_string()))?,
        },
        "mcp.call" => ControlRequest::McpCall {
            server: str_param(params, "server")?.ok_or((-32602, "missing string 'server'".to_string()))?,
            tool: str_param(params, "tool")?.ok_or((-32602, "missing string 'tool'".to_string()))?,
            args: params.get("arguments").cloned().unwrap_or_else(|| json!({})),
        },
        "patterns.get" => {
            ControlRequest::Patterns { hours: params.get("hours").and_then(Value::as_i64).unwrap_or(24 * 7) }
        }
        // surface.subscribe is handled per connection, see handle_connection.
        other => return Err((-32601, format!("unknown method '{other}'"))),
    })
}

fn str_param(params: &Value, key: &str) -> Result<Option<String>, (i64, String)> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err((-32602, format!("'{key}' must be a string"))),
    }
}

/// The agent's self-reported name, shown to the user next to its proposal.
fn client_name(params: &Value) -> String {
    let raw = params.get("client").and_then(Value::as_str).unwrap_or("an agent");
    let clean: String = raw.chars().filter(|c| !c.is_control()).take(40).collect();
    if clean.trim().is_empty() { "an agent".into() } else { clean }
}

/// Binds the socket, refusing to start if another daemon is listening.
pub async fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        crate::paths::private_dir(&dir.to_path_buf())?;
    }
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!("another ronakd is already listening on {}", path.display());
        }
        std::fs::remove_file(path).ok(); // stale socket from a crash
    }
    let listener = UnixListener::bind(path).with_context(|| format!("cannot bind {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    info!(socket = %path.display(), "control socket ready");
    Ok(listener)
}

async fn serve(listener: UnixListener, daemon: mpsc::Sender<ControlMsg>) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, daemon).await {
                debug!("control connection ended: {e}");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, daemon: mpsc::Sender<ControlMsg>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    // Responses and pushed notifications share one writer.
    let (out_tx, mut out_rx) = mpsc::channel::<Value>(32);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write.write_all(format!("{msg}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(req) if req.get("method").and_then(Value::as_str) == Some("surface.subscribe") => {
                subscribe(&req, &daemon, out_tx.clone()).await
            }
            Ok(req) => dispatch(&req, &daemon).await,
            Err(e) => json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }),
        };
        if out_tx.send(response).await.is_err() {
            break;
        }
    }
    drop(out_tx);
    writer.abort();
    Ok(())
}

/// Registers this connection for `surfaces` notifications.
async fn subscribe(req: &Value, daemon: &mpsc::Sender<ControlMsg>, out: mpsc::Sender<Value>) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let (note_tx, mut note_rx) = mpsc::channel::<Value>(8);
    // The daemon only ever holds `note_tx`; forward from it to this connection.
    tokio::spawn(async move {
        while let Some(n) = note_rx.recv().await {
            if out.send(n).await.is_err() {
                break;
            }
        }
    });
    let (reply, rx) = oneshot::channel();
    if daemon.send(ControlMsg { request: ControlRequest::Subscribe(note_tx), reply }).await.is_err() {
        return json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "daemon is shutting down" } });
    }
    match rx.await {
        Ok(Ok(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Ok(Err(msg)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": msg } }),
        Err(_) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "daemon dropped the request" } })
        }
    }
}

async fn dispatch(req: &Value, daemon: &mpsc::Sender<ControlMsg>) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let error = |code: i64, message: String| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    let request = match parse_request(method, &params) {
        Ok(r) => r,
        Err((code, msg)) => return error(code, msg),
    };
    let (reply, rx) = oneshot::channel();
    if daemon.send(ControlMsg { request, reply }).await.is_err() {
        return error(-32000, "daemon is shutting down".into());
    }
    match rx.await {
        Ok(Ok(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Ok(Err(msg)) => error(-32000, msg),
        Err(_) => error(-32000, "daemon dropped the request".into()),
    }
}

/// Client side: one request, one response.
pub async fn call(path: &Path, method: &str, params: Value) -> Result<Value> {
    let stream = UnixStream::connect(path).await.with_context(|| {
        format!(
            "ronakd is not running (no socket at {}). Start it with `ronakd run` or `systemctl --user start ronakd`.",
            path.display()
        )
    })?;
    let (read, mut write) = stream.into_split();
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    write.write_all(format!("{request}\n").as_bytes()).await?;
    let mut lines = BufReader::new(read).lines();
    let line = lines.next_line().await?.context("ronakd closed the connection")?;
    let response: Value = serde_json::from_str(&line)?;
    if let Some(err) = response.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
        bail!("{msg}");
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

/// Serves requests on a bound socket in the background.
pub fn spawn(listener: UnixListener, daemon: mpsc::Sender<ControlMsg>) {
    tokio::spawn(async move {
        if let Err(e) = serve(listener, daemon).await {
            warn!("control socket stopped: {e:#}");
        }
    });
}
