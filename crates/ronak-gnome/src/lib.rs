//! # ronak-gnome
//!
//! The daemon side of the GNOME adapter. On Wayland, only the compositor can
//! see and move every window, so the "eyes and hands" live in a GNOME Shell
//! extension running inside `gnome-shell`. This crate is the other end of
//! the wire.
//!
//! Two well-known names on the D-Bus session bus:
//!
//! ```text
//!   ronakd                              gnome-shell (+ Ronak extension)
//!   owns dev.ronak.Daemon1              owns dev.ronak.Shell1
//!     Hello(s) -> s      ◄────────────    on startup / when ronakd appears
//!     PushEvents(s)      ◄────────────    window events, focus, activity
//!                        ────────────►    Execute(s) -> s    place, focus, DND
//!                        ────────────►    GetSnapshot() -> s
//! ```
//!
//! Payloads are JSON strings matching `schema/adapter-protocol.schema.json`,
//! so the open schema stays the single source of truth.
//!
//! **Peer verification.** Every incoming call, and every outgoing command,
//! checks that the process on the other end is the real `gnome-shell`
//! binary: the bus tells us the peer's process id, and `/proc/<pid>/exe`
//! tells us which executable it runs. The extension does the same check on
//! us. Events are sent as direct method calls rather than broadcast signals,
//! so no other process on the bus can listen in.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use futures_util::StreamExt;
use ronak_core::adapter::{AdapterHandle, AdapterRequest};
use ronak_core::protocol::{AdapterCommand, AdapterConfig, CommandResult, PROTOCOL_VERSION, PlatformEvent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::fdo::{self, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::{Connection, interface, proxy};

pub const DAEMON_BUS_NAME: &str = "dev.ronak.Daemon1";
pub const DAEMON_OBJECT_PATH: &str = "/dev/ronak/Daemon1";
pub const SHELL_BUS_NAME: &str = "dev.ronak.Shell1";
pub const SHELL_OBJECT_PATH: &str = "/dev/ronak/Shell1";

#[derive(Debug, thiserror::Error)]
pub enum GnomeError {
    #[error("D-Bus error: {0}")]
    Bus(#[from] zbus::Error),
    #[error("another ronakd already owns {DAEMON_BUS_NAME} on this session bus")]
    AlreadyRunning,
}

#[derive(Debug, Clone)]
pub struct GnomeOptions {
    /// Executables allowed on the other end (normally /usr/bin/gnome-shell).
    pub trusted_shell_paths: Vec<PathBuf>,
    pub verify_peers: bool,
    /// Sent to the extension in reply to its Hello.
    pub config: AdapterConfig,
}

#[proxy(interface = "dev.ronak.Shell1", default_service = "dev.ronak.Shell1", default_path = "/dev/ronak/Shell1")]
trait Shell1 {
    fn execute(&self, command: &str) -> zbus::Result<String>;
    fn get_snapshot(&self) -> zbus::Result<String>;
}

/// Remembers which bus peers have been verified so `/proc` is read once per peer.
struct PeerVerifier {
    trusted: Vec<PathBuf>,
    enabled: bool,
    verified: Mutex<HashSet<String>>,
}

impl PeerVerifier {
    async fn check(&self, conn: &Connection, peer: &UniqueName<'_>) -> fdo::Result<()> {
        if !self.enabled || self.verified.lock().unwrap().contains(peer.as_str()) {
            return Ok(());
        }
        let dbus = fdo::DBusProxy::new(conn).await?;
        let pid = dbus.get_connection_unix_process_id(BusName::Unique(peer.to_owned())).await?;
        let exe = executable_of(pid).ok_or_else(|| fdo::Error::AccessDenied(format!("cannot read /proc/{pid}/exe")))?;
        if self.trusted.iter().any(|t| t == &exe) {
            debug!(peer = peer.as_str(), exe = %exe.display(), "verified peer");
            self.verified.lock().unwrap().insert(peer.to_string());
            Ok(())
        } else {
            warn!(peer = peer.as_str(), exe = %exe.display(), "rejected untrusted peer");
            Err(fdo::Error::AccessDenied(format!("{} is not a trusted shell", exe.display())))
        }
    }
}

/// The executable a process runs, via the `/proc/<pid>/exe` symlink.
/// A " (deleted)" suffix appears when the binary was replaced on disk
/// (for example by a package upgrade) while still running.
fn executable_of(pid: u32) -> Option<PathBuf> {
    let target = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let s = target.to_string_lossy();
    Some(PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s)))
}

/// The object the extension calls into.
struct DaemonService {
    events: mpsc::Sender<PlatformEvent>,
    config: Arc<RwLock<AdapterConfig>>,
    verifier: Arc<PeerVerifier>,
}

fn sender<'h>(hdr: &'h Header<'_>) -> fdo::Result<&'h UniqueName<'h>> {
    hdr.sender().ok_or_else(|| fdo::Error::AccessDenied("message has no sender".into()))
}

#[interface(name = "dev.ronak.Daemon1")]
impl DaemonService {
    /// First call from the extension. Takes a `hello` event, returns the
    /// adapter configuration (privacy rules, sampling intervals).
    async fn hello(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        hello: String,
    ) -> fdo::Result<String> {
        self.verifier.check(conn, sender(&hdr)?).await?;
        let event: PlatformEvent = serde_json::from_str(&hello).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        let PlatformEvent::Hello { protocol_version, .. } = &event else {
            return Err(fdo::Error::InvalidArgs("expected a hello event".into()));
        };
        if *protocol_version != PROTOCOL_VERSION {
            return Err(fdo::Error::NotSupported(format!(
                "extension speaks protocol {protocol_version}, daemon speaks {PROTOCOL_VERSION}; update both"
            )));
        }
        info!("GNOME Shell extension connected");
        self.events.send(event).await.map_err(|_| fdo::Error::Failed("daemon is shutting down".into()))?;
        let config = self.config.read().unwrap().clone();
        serde_json::to_string(&config).map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// A JSON array of events (a single event object is also accepted).
    async fn push_events(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        events: String,
    ) -> fdo::Result<()> {
        self.verifier.check(conn, sender(&hdr)?).await?;
        let parsed: Vec<PlatformEvent> = match serde_json::from_str::<serde_json::Value>(&events) {
            Ok(v @ serde_json::Value::Array(_)) => serde_json::from_value(v),
            Ok(v) => serde_json::from_value(v).map(|e| vec![e]),
            Err(e) => Err(e),
        }
        .map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        for event in parsed {
            self.events.send(event).await.map_err(|_| fdo::Error::Failed("daemon is shutting down".into()))?;
        }
        Ok(())
    }

    #[zbus(property)]
    async fn protocol_version(&self) -> u32 {
        PROTOCOL_VERSION
    }
}

/// Connects to the session bus, claims `dev.ronak.Daemon1`, and starts the
/// adapter tasks.
pub async fn spawn(opts: GnomeOptions) -> Result<AdapterHandle, GnomeError> {
    let (event_tx, event_rx) = mpsc::channel(1024);
    let (command_tx, command_rx) = mpsc::channel(64);
    let config = Arc::new(RwLock::new(opts.config.clone()));
    let verifier = Arc::new(PeerVerifier {
        trusted: opts.trusted_shell_paths.clone(),
        enabled: opts.verify_peers,
        verified: Mutex::new(HashSet::new()),
    });
    if !opts.verify_peers {
        warn!("peer verification is OFF; any process on the session bus can talk to ronakd");
    }

    let service = DaemonService { events: event_tx.clone(), config: config.clone(), verifier: verifier.clone() };
    let conn = zbus::connection::Builder::session()?.serve_at(DAEMON_OBJECT_PATH, service)?.build().await?;
    // DoNotQueue without ReplaceExisting: never steal the name from a running daemon.
    match conn.request_name_with_flags(DAEMON_BUS_NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(_) | Err(zbus::Error::NameTaken) => return Err(GnomeError::AlreadyRunning),
        Err(e) => return Err(e.into()),
    }
    info!(name = DAEMON_BUS_NAME, "listening on the session bus; waiting for the GNOME Shell extension");

    tokio::spawn(command_loop(conn.clone(), command_rx, config, verifier));
    tokio::spawn(watch_shell(conn, event_tx));
    Ok(AdapterHandle { name: "gnome", events: event_rx, commands: command_tx })
}

async fn command_loop(
    conn: Connection,
    mut rx: mpsc::Receiver<AdapterRequest>,
    config: Arc<RwLock<AdapterConfig>>,
    verifier: Arc<PeerVerifier>,
) {
    while let Some(AdapterRequest { command, reply }) = rx.recv().await {
        if let AdapterCommand::Configure { config: c } = &command {
            *config.write().unwrap() = c.clone();
        }
        let result = match send_to_shell(&conn, &verifier, &command).await {
            Ok(r) => r,
            // Configuration is also delivered at the next Hello, so a missing
            // extension is not an error for it.
            Err(_) if matches!(command, AdapterCommand::Configure { .. }) => CommandResult::ok(),
            Err(e) => CommandResult::error(e),
        };
        let _ = reply.send(result);
    }
}

async fn send_to_shell(
    conn: &Connection,
    verifier: &PeerVerifier,
    command: &AdapterCommand,
) -> Result<CommandResult, String> {
    let dbus = fdo::DBusProxy::new(conn).await.map_err(|e| e.to_string())?;
    let owner = dbus
        .get_name_owner(BusName::try_from(SHELL_BUS_NAME).expect("valid name"))
        .await
        .map_err(|_| "the Ronak GNOME Shell extension is not running".to_string())?;
    verifier.check(conn, &owner).await.map_err(|e| e.to_string())?;
    let proxy = Shell1Proxy::new(conn).await.map_err(|e| e.to_string())?;
    let payload = serde_json::to_string(command).map_err(|e| e.to_string())?;
    let reply = proxy.execute(&payload).await.map_err(|e| e.to_string())?;
    serde_json::from_str(&reply).map_err(|e| format!("bad reply from extension: {e}"))
}

/// Reports `Gone` when the extension's bus name disappears (shell restart,
/// extension disabled, logout).
async fn watch_shell(conn: Connection, events: mpsc::Sender<PlatformEvent>) {
    let Ok(dbus) = fdo::DBusProxy::new(&conn).await else { return };
    let Ok(mut changes) = dbus.receive_name_owner_changed_with_args(&[(0, SHELL_BUS_NAME)]).await else {
        warn!("cannot watch {SHELL_BUS_NAME}; disconnects will not be noticed");
        return;
    };
    while let Some(signal) = changes.next().await {
        if let Ok(args) = signal.args()
            && args.new_owner().is_none()
        {
            info!("GNOME Shell extension disconnected");
            if events.send(PlatformEvent::Gone).await.is_err() {
                return;
            }
        }
    }
}

/// True when this looks like a GNOME session.
pub fn is_gnome_session() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").map(|d| d.to_ascii_lowercase().contains("gnome")).unwrap_or(false)
        || Path::new("/usr/bin/gnome-shell").exists() && std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_executable() {
        let me = executable_of(std::process::id()).unwrap();
        assert_eq!(me, std::env::current_exe().unwrap());
    }
}
