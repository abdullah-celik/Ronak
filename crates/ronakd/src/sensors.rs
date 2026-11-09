//! Sensors beyond the window adapter: the accessibility tree (elements) and
//! context capabilities (media, calendar, MCP servers), plus the CPU-priority
//! actuator.
//!
//! Everything here runs in background tasks and reports through one channel,
//! so a slow or frozen app can never stall the daemon's main loop.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use ronak_atspi::{AppReading, AtspiSensor, ReadOptions};
use ronak_caps::calendar::CalendarReader;
use ronak_caps::mcp::McpClient;
use ronak_caps::media::{self, MediaCommand};
use ronak_core::config::Config;
use ronak_core::model::{Access, CalendarState, Capability, CapabilityKind, McpState};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

#[derive(Debug)]
pub enum SensorUpdate {
    Elements { window_id: String, app_id: Option<String>, app_name: String, reading: Result<AppReading, String> },
    Media(Vec<Capability>),
    Calendar(Result<CalendarState, String>),
    Mcp(Vec<Capability>),
}

pub type McpClients = Arc<Mutex<Vec<(String, McpClient)>>>;

pub struct Sensors {
    atspi: Option<Arc<AtspiSensor>>,
    session: Option<zbus::Connection>,
    mcp: McpClients,
    tx: mpsc::Sender<SensorUpdate>,
}

impl Sensors {
    /// No sensors at all (tests, or a platform without D-Bus).
    pub fn disabled(tx: mpsc::Sender<SensorUpdate>) -> Self {
        Self { atspi: None, session: None, mcp: Arc::new(Mutex::new(vec![])), tx }
    }

    pub async fn start(cfg: &Config, tx: mpsc::Sender<SensorUpdate>) -> Self {
        let atspi = if cfg.elements.enabled {
            match AtspiSensor::connect().await {
                Ok(s) => {
                    info!("accessibility bus connected; reading inside apps is on");
                    Some(Arc::new(s))
                }
                Err(e) => {
                    warn!("reading inside apps is off: {e}");
                    None
                }
            }
        } else {
            None
        };
        let session = zbus::Connection::session().await.map_err(|e| warn!("no session bus for capabilities: {e}")).ok();
        let sensors = Self { atspi, session, mcp: Arc::new(Mutex::new(vec![])), tx };
        sensors.spawn_polling(cfg);
        sensors.spawn_mcp(cfg);
        sensors
    }

    fn spawn_polling(&self, cfg: &Config) {
        let Some(conn) = self.session.clone() else { return };
        let media_access = cfg.capabilities.media;
        let calendar_access = cfg.capabilities.calendar;
        let calendar_every = Duration::from_secs(cfg.capabilities.refresh_secs.max(10));
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut calendar = (calendar_access != Access::Off).then(|| CalendarReader::new(conn.clone()));
            let mut media_tick = tokio::time::interval(Duration::from_secs(5));
            let mut calendar_tick = tokio::time::interval(calendar_every);
            loop {
                tokio::select! {
                    _ = media_tick.tick(), if media_access != Access::Off => {
                        match media::players(&conn, media_access).await {
                            Ok(players) => { if tx.send(SensorUpdate::Media(players)).await.is_err() { return; } }
                            Err(e) => debug!("media players unavailable: {e}"),
                        }
                    }
                    _ = calendar_tick.tick(), if calendar.is_some() => {
                        let reader = calendar.as_mut().expect("checked");
                        let result = reader.state(Utc::now()).await.map_err(|e| e.to_string());
                        if tx.send(SensorUpdate::Calendar(result)).await.is_err() { return; }
                    }
                }
            }
        });
    }

    fn spawn_mcp(&self, cfg: &Config) {
        if cfg.capabilities.mcp == Access::Off || cfg.capabilities.mcp_servers.is_empty() {
            return;
        }
        let servers = cfg.capabilities.mcp_servers.clone();
        let access = cfg.capabilities.mcp;
        let clients = self.mcp.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut caps = vec![];
            for s in servers {
                let (connected, tools) = match McpClient::connect(&s.command, &s.args).await {
                    Ok((client, info)) => {
                        info!(server = s.name, tools = info.tools.len(), "connected to MCP server");
                        let tools = info.tools.clone();
                        clients.lock().await.push((s.name.clone(), client));
                        (true, tools)
                    }
                    Err(e) => {
                        warn!(server = s.name, "MCP server unavailable: {e}");
                        (false, vec![])
                    }
                };
                caps.push(Capability {
                    id: format!("mcp:{}", s.name),
                    kind: CapabilityKind::Mcp,
                    provider: s.command.clone(),
                    name: s.name.clone(),
                    access,
                    actions: if access == Access::Act { tools.clone() } else { vec![] },
                    media: None,
                    calendar: None,
                    notifications: None,
                    mcp: Some(McpState { connected, tools }),
                });
            }
            let _ = tx.send(SensorUpdate::Mcp(caps)).await;
        });
    }

    pub fn reads_elements(&self) -> bool {
        self.atspi.is_some()
    }

    /// Reads the app behind a window in the background.
    #[allow(clippy::too_many_arguments)]
    pub fn read_elements(
        &self,
        window_id: String,
        pid: u32,
        title: Option<String>,
        app_id: Option<String>,
        app_name: String,
        opts: ReadOptions,
    ) -> bool {
        let Some(atspi) = self.atspi.clone() else { return false };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let reading =
                atspi.read_pid(pid, title.as_deref(), Some(&window_id), &opts).await.map_err(|e| e.to_string());
            let _ = tx.send(SensorUpdate::Elements { window_id, app_id, app_name, reading }).await;
        });
        true
    }

    pub fn media_control(&self, provider: String, command: MediaCommand) {
        let Some(conn) = self.session.clone() else { return };
        tokio::spawn(async move {
            if let Err(e) = media::control(&conn, &provider, command).await {
                warn!("media control failed: {e}");
            }
        });
    }

    pub fn set_cpu_weight(&self, unit: String, weight: u64) {
        let Some(conn) = self.session.clone() else { return };
        tokio::spawn(async move {
            match ronak_caps::cpu::set_cpu_weight(&conn, &unit, weight).await {
                Ok(()) => debug!(unit, weight, "CPU weight set"),
                Err(e) => debug!(unit, "could not set CPU weight: {e}"),
            }
        });
    }

    /// A handle for calling MCP tools outside the daemon loop (a call can
    /// take seconds and must not stall it).
    pub fn mcp_clients(&self) -> McpClients {
        self.mcp.clone()
    }
}

/// Calls a tool on a connected MCP server. The daemon checks `act` access first.
pub async fn call_mcp_tool(clients: McpClients, server: &str, tool: &str, args: Value) -> Result<Value, String> {
    let mut clients = clients.lock().await;
    let (_, client) = clients.iter_mut().find(|(n, _)| n == server).ok_or("no such MCP server")?;
    client.call_tool(tool, args).await.map_err(|e| e.to_string())
}

/// Switches on the session-wide accessibility flag (what screen readers do),
/// so apps such as Chromium and Electron expose their trees after a restart.
pub async fn enable_accessibility() -> Result<(), String> {
    ronak_atspi::set_session_accessibility(true).await.map_err(|e| e.to_string())
}
