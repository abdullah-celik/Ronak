//! The control socket: how the CLI and the MCP bridge talk to the running
//! daemon.
//!
//! A Unix domain socket is a file-like endpoint for local inter-process
//! communication. It lives in `$XDG_RUNTIME_DIR/ronak/` (a per-user tmpfs) in
//! a directory only you can enter, so other users cannot connect. The wire
//! format is JSON-RPC 2.0, one JSON object per line. On Windows this will be
//! a named pipe with the same protocol.

use std::path::Path;

use anyhow::{Context, Result, bail};
use ronak_core::model::DetailLevel;
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
        other => return Err((-32601, format!("unknown method '{other}'"))),
    })
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
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(req) => dispatch(&req, &daemon).await,
            Err(e) => json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }),
        };
        write.write_all(format!("{response}\n").as_bytes()).await?;
    }
    Ok(())
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
