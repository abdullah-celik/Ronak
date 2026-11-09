//! The handle every platform adapter returns.
//!
//! Adapters are tasks that talk to one desktop (GNOME via D-Bus, a mock, and
//! later Windows via Win32). They never call into the daemon directly. They
//! send [`PlatformEvent`]s down one channel and accept [`AdapterRequest`]s on
//! another. This keeps the daemon free of `dyn` traits and of any knowledge
//! about which desktop it is driving.

use tokio::sync::{mpsc, oneshot};

use crate::protocol::{AdapterCommand, CommandResult, PlatformEvent};

/// A command plus the channel its result comes back on.
#[derive(Debug)]
pub struct AdapterRequest {
    pub command: AdapterCommand,
    pub reply: oneshot::Sender<CommandResult>,
}

/// Owned by the daemon. Dropping it shuts the adapter down.
#[derive(Debug)]
pub struct AdapterHandle {
    /// Short adapter name for logs, e.g. "gnome" or "mock".
    pub name: &'static str,
    pub events: mpsc::Receiver<PlatformEvent>,
    pub commands: mpsc::Sender<AdapterRequest>,
}

/// Sends a command and waits for the result. A closed adapter is reported as
/// an error result rather than a panic.
pub async fn send(commands: &mpsc::Sender<AdapterRequest>, command: AdapterCommand) -> CommandResult {
    let (reply, rx) = oneshot::channel();
    if commands.send(AdapterRequest { command, reply }).await.is_err() {
        return CommandResult::error("adapter is not running");
    }
    rx.await.unwrap_or_else(|_| CommandResult::error("adapter dropped the request"))
}
