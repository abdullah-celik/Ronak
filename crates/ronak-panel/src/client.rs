//! Talking to the daemon over its control socket (JSON-RPC 2.0, one message
//! per line). One long-lived connection receives `surfaces` notifications;
//! each button press uses its own short connection, off the UI thread.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::model::{PanelUpdate, parse_update};

/// Same rules as the daemon: `RONAK_RUNTIME_DIR`, else
/// `$XDG_RUNTIME_DIR/ronak`, else a per-user temp directory.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RONAK_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = std::env::var_os("RONAK_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("ronak")).unwrap_or_else(|| {
            std::env::temp_dir().join(format!("ronak-{}", std::env::var("USER").unwrap_or_else(|_| "user".into())))
        })
    });
    dir.join("control.sock")
}

pub enum Incoming {
    Update(PanelUpdate),
    Offline(String),
}

/// Subscribes in a background thread and reconnects whenever the daemon
/// restarts.
pub fn listen(socket: PathBuf, tx: async_channel::Sender<Incoming>) {
    std::thread::spawn(move || {
        loop {
            let reason = match subscribe_once(&socket, &tx) {
                Ok(()) => "the daemon closed the connection".to_string(),
                Err(e) => e,
            };
            if tx.send_blocking(Incoming::Offline(reason)).is_err() {
                return; // the window is gone
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

fn subscribe_once(socket: &PathBuf, tx: &async_channel::Sender<Incoming>) -> Result<(), String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("cannot reach ronakd: {e}"))?;
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "surface.subscribe" });
    stream.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(err) = msg.get("error") {
            return Err(err.get("message").and_then(Value::as_str).unwrap_or("error").to_string());
        }
        if msg.get("method").and_then(Value::as_str) == Some("surfaces") {
            match parse_update(&msg) {
                Ok(u) => {
                    if tx.send_blocking(Incoming::Update(u)).is_err() {
                        return Ok(());
                    }
                }
                Err(e) => eprintln!("ronak-panel: ignored a message: {e}"),
            }
        }
    }
    Ok(())
}

/// One request, one response. Blocking; call it from a worker thread.
pub fn call(socket: &PathBuf, method: &str, params: Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("cannot reach ronakd: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    stream.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let msg: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    match msg.get("error") {
        Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
    }
}
