//! `ronakd`: the Ronak daemon and its command-line interface.
//!
//! ```text
//! ronakd run                 start the daemon (normally via systemd)
//! ronakd status              what the daemon sees right now
//! ronakd context             the current context snapshot (JSON)
//! ronakd suggest | apply | dismiss | undo
//! ronakd pause | resume      stop / restart recording
//! ronakd correct LABEL       "that's not what I'm doing": switch and learn
//! ronakd panel [show|hide]   toggle the Ronak panel (also Super+Alt+Space)
//! ronakd patterns            what Ronak condensed from the last days
//! ronakd call SERVER TOOL    call a tool on an MCP server you configured
//! ronakd probe               NPUs, GPUs, CPU and where models would run
//! ronakd mcp                 MCP server on stdio, for AI agents
//! ronakd schema              print the context JSON Schema
//! ronakd config              print the effective configuration
//! ```

mod control;
mod daemon;
mod llm;
mod mcp;
mod mock;
mod paths;
mod sensors;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ronak_core::adapter::AdapterHandle;
use ronak_core::config::{Config, Encryption};
use ronak_core::model::DetailLevel;
use ronak_store::Store;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "ronakd", version, about = "Ronak: a local-first, intent-aware context layer for your desktop")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon.
    Run {
        /// Which desktop to connect to.
        #[arg(long, value_enum, default_value_t = Platform::Auto)]
        platform: Platform,
        /// Mock only: accept every suggestion automatically.
        #[arg(long)]
        auto_accept: bool,
        /// Mock only: milliseconds per script beat.
        #[arg(long, default_value_t = 1000)]
        mock_beat_ms: u64,
    },
    /// Show what the daemon sees right now.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Print the current context snapshot as JSON.
    Context {
        #[arg(long, value_enum, default_value_t = Detail::TaskAndApps)]
        detail: Detail,
    },
    /// Show the current layout suggestion.
    Suggest,
    /// Apply the current suggestion.
    Apply,
    /// Dismiss the current suggestion.
    Dismiss,
    /// Undo the last change Ronak made.
    Undo,
    /// Stop recording and suggesting.
    Pause,
    /// Resume recording and suggesting.
    Resume,
    /// Tell Ronak what you are actually doing. It switches now and saves the
    /// correction as a label it learns from.
    Correct {
        /// A task such as coding, writing, meeting, or your own (grant_writing).
        label: String,
        /// Optional finer activity such as debugging or reviewing.
        #[arg(long)]
        activity: Option<String>,
        /// Optional project name.
        #[arg(long)]
        project: Option<String>,
    },
    /// Show, hide or toggle the Ronak panel.
    Panel {
        #[arg(value_enum, default_value_t = PanelArg::Toggle)]
        state: PanelArg,
    },
    /// Patterns condensed from what you did (content is kept 24 hours, then
    /// only these summaries remain).
    Patterns {
        /// How far back, in hours.
        #[arg(long, default_value_t = 24 * 7)]
        hours: i64,
    },
    /// Call a tool on an MCP server from your config (needs
    /// capabilities.mcp = "act").
    Call {
        server: String,
        tool: String,
        /// Tool arguments as a JSON object.
        #[arg(default_value = "{}")]
        arguments: String,
    },
    /// Send one raw request to the control socket (for development and tests).
    #[command(hide = true)]
    Rpc {
        method: String,
        #[arg(default_value = "null")]
        params: String,
    },
    /// Detect NPUs, GPUs and CPU features and show where models would run.
    Probe {
        #[arg(long)]
        json: bool,
    },
    /// Serve the Model Context Protocol on stdin/stdout.
    Mcp,
    /// Print a JSON Schema.
    Schema {
        #[arg(value_enum, default_value_t = SchemaKind::Context)]
        which: SchemaKind,
    },
    /// Print the effective configuration (defaults merged with your file).
    Config,
}

#[derive(Clone, Copy, ValueEnum)]
enum Platform {
    Auto,
    Gnome,
    Mock,
}

#[derive(Clone, Copy, ValueEnum)]
enum PanelArg {
    Toggle,
    Show,
    Hide,
}

#[derive(Clone, Copy, ValueEnum)]
enum Detail {
    TaskOnly,
    TaskAndApps,
    Full,
    Content,
}

impl From<Detail> for DetailLevel {
    fn from(d: Detail) -> Self {
        match d {
            Detail::TaskOnly => DetailLevel::TaskOnly,
            Detail::TaskAndApps => DetailLevel::TaskAndApps,
            Detail::Full => DetailLevel::Full,
            Detail::Content => DetailLevel::Content,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SchemaKind {
    Context,
    Protocol,
    Surface,
}

fn init_logging() {
    // Logs go to stderr: systemd sends them to the journal, and stdout stays
    // clean for the MCP protocol.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging();
    let socket = paths::control_socket();
    match cli.command {
        Command::Run { platform, auto_accept, mock_beat_ms } => run(platform, auto_accept, mock_beat_ms).await,
        Command::Status { json } => {
            let status = control::call(&socket, "status", Value::Null).await?;
            if json {
                print_json(&status)
            } else {
                print_status(&status)
            }
            Ok(())
        }
        Command::Context { detail } => {
            let detail: DetailLevel = detail.into();
            print_json(&control::call(&socket, "context.get", json!({ "detail": detail })).await?);
            Ok(())
        }
        Command::Suggest => {
            print_json(&control::call(&socket, "suggestion.get", json!({ "detail": DetailLevel::Full })).await?);
            Ok(())
        }
        Command::Apply => simple(&socket, "suggestion.apply", Value::Null).await,
        Command::Dismiss => simple(&socket, "suggestion.dismiss", Value::Null).await,
        Command::Undo => simple(&socket, "undo", Value::Null).await,
        Command::Pause => simple(&socket, "observe.set", json!({ "observing": false })).await,
        Command::Resume => simple(&socket, "observe.set", json!({ "observing": true })).await,
        Command::Correct { label, activity, project } => {
            simple(&socket, "intent.correct", json!({ "label": label, "activity": activity, "project": project })).await
        }
        Command::Panel { state } => {
            let visible = match state {
                PanelArg::Toggle => Value::Null,
                PanelArg::Show => json!(true),
                PanelArg::Hide => json!(false),
            };
            simple(&socket, "panel.set", json!({ "visible": visible })).await
        }
        Command::Patterns { hours } => simple(&socket, "patterns.get", json!({ "hours": hours })).await,
        Command::Call { server, tool, arguments } => {
            let arguments: Value = serde_json::from_str(&arguments).context("arguments must be a JSON object")?;
            simple(&socket, "mcp.call", json!({ "server": server, "tool": tool, "arguments": arguments })).await
        }
        Command::Rpc { method, params } => {
            let params: Value = serde_json::from_str(&params).context("params must be JSON")?;
            simple(&socket, &method, params).await
        }
        Command::Probe { json } => {
            let report = hardware_report().await;
            if json {
                print_json(&report)
            } else {
                print_probe(&report)
            }
            Ok(())
        }
        Command::Mcp => mcp::McpServer::new(paths::load_config()?, socket).serve_stdio().await,
        Command::Schema { which } => {
            print!(
                "{}",
                match which {
                    SchemaKind::Context => include_str!("../../../schema/context.schema.json"),
                    SchemaKind::Protocol => include_str!("../../../schema/adapter-protocol.schema.json"),
                    SchemaKind::Surface => include_str!("../../../schema/surface.schema.json"),
                }
            );
            Ok(())
        }
        Command::Config => {
            println!("# effective configuration (file: {})", paths::config_file().display());
            print!("{}", toml::to_string_pretty(&paths::load_config()?)?);
            Ok(())
        }
    }
}

async fn simple(socket: &std::path::Path, method: &str, params: Value) -> Result<()> {
    print_json(&control::call(socket, method, params).await?);
    Ok(())
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

async fn hardware_report() -> Value {
    let profile = ronak_hw::probe();
    let servers = llm::detect_servers().await;
    let plan = ronak_hw::plan(&profile, &servers);
    json!({ "profile": profile, "servers": servers, "plan": plan })
}

async fn run(platform: Platform, auto_accept: bool, mock_beat_ms: u64) -> Result<()> {
    let cfg = paths::load_config()?;
    for w in daemon::sanity_check(&cfg) {
        warn!("{w}");
    }

    // Storage: encrypted with a key from the OS keyring, unless disabled.
    let data_dir = paths::data_dir(&cfg);
    paths::private_dir(&data_dir)?;
    let store = open_store(&cfg, &data_dir)?;

    // Control socket first, so a second instance fails fast.
    let listener = control::bind(&paths::control_socket()).await?;

    let hardware = hardware_report().await;
    info!(
        classifier = %hardware["plan"]["classifier"]["device"], llm = %hardware["plan"]["llm"]["device"],
        "hardware plan (see `ronakd probe`)"
    );

    let llm_tier = if cfg.intent.llm.enabled {
        match llm::LlmTier::new(cfg.intent.llm.clone()) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!("LLM tier disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    let adapter = start_adapter(platform, &cfg, auto_accept, mock_beat_ms).await?;
    let AdapterHandle { name, events, commands } = adapter;
    let simulated = matches!(platform, Platform::Mock);

    // Sensors inside apps and context capabilities. The simulated desktop has
    // no real apps to read, so it runs without them.
    let (sensor_tx, sensor_rx) = mpsc::channel(32);
    let sensors =
        if simulated { sensors::Sensors::disabled(sensor_tx) } else { sensors::Sensors::start(&cfg, sensor_tx).await };

    let (control_tx, control_rx) = mpsc::channel(32);
    control::spawn(listener, control_tx);
    let (llm_tx, llm_rx) = mpsc::channel(4);
    if cfg.surfaces.enabled && !simulated {
        spawn_panel();
    }
    let start_visible = cfg.surfaces.start_visible;
    let daemon = daemon::Daemon::new(cfg, store, commands, name, hardware, llm_tier, llm_tx, sensors)
        .with_panel_visible(start_visible);
    daemon.run(events, control_rx, llm_rx, sensor_rx, shutdown_signal()).await;
    std::fs::remove_file(paths::control_socket()).ok();
    Ok(())
}

/// Starts the panel app (`ronak-panel`, installed next to `ronakd`) and
/// restarts it if it exits, with a growing pause so a crash loop stays cheap.
fn spawn_panel() {
    let Some(path) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("ronak-panel"))) else {
        return;
    };
    if !path.exists() {
        info!("panel app not installed ({}); surfaces are available over the control socket only", path.display());
        return;
    }
    tokio::spawn(async move {
        let mut pause = Duration::from_secs(1);
        loop {
            let started = std::time::Instant::now();
            match tokio::process::Command::new(&path).kill_on_drop(true).status().await {
                Ok(status) => info!("panel exited ({status})"),
                Err(e) => {
                    warn!("could not start the panel: {e}");
                    return;
                }
            }
            if started.elapsed() > Duration::from_secs(60) {
                pause = Duration::from_secs(1);
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(60));
        }
    });
}

fn open_store(cfg: &Config, data_dir: &std::path::Path) -> Result<Store> {
    match cfg.storage.encryption {
        Encryption::Keyring => {
            let key = ronak_store::keyring_key("ronak", "database-key").context(
                "could not get the database key from the keyring (Secret Service). Is GNOME Keyring running and unlocked? \
                 For development only, RONAK_ENCRYPTION=none uses an unencrypted database instead",
            )?;
            Ok(Store::open(&data_dir.join("ronak.db"), Some(&key))?)
        }
        Encryption::None => {
            warn!("storage encryption is OFF (development mode)");
            Ok(Store::open(&data_dir.join("ronak-unencrypted.db"), None)?)
        }
    }
}

async fn start_adapter(platform: Platform, cfg: &Config, auto_accept: bool, beat_ms: u64) -> Result<AdapterHandle> {
    let mock =
        || mock::spawn(mock::MockOptions { auto_accept, beat: Duration::from_millis(beat_ms.max(10)), once: false });
    match platform {
        Platform::Mock => Ok(mock()),
        Platform::Gnome => gnome(cfg).await,
        Platform::Auto => {
            #[cfg(target_os = "linux")]
            if ronak_gnome::is_gnome_session() {
                return gnome(cfg).await;
            }
            bail!(
                "no supported desktop detected. Supported today: GNOME. Use `--platform mock` to try Ronak with a simulated desktop."
            )
        }
    }
}

#[cfg(target_os = "linux")]
async fn gnome(cfg: &Config) -> Result<AdapterHandle> {
    let opts = ronak_gnome::GnomeOptions {
        trusted_shell_paths: cfg.gnome.trusted_shell_paths.iter().map(Into::into).collect(),
        verify_peers: cfg.gnome.verify_peers,
        config: ronak_core::protocol::AdapterConfig {
            exclusions: cfg.observe.effective_exclusions(),
            observe_titles: cfg.observe.titles,
            activity_interval_ms: cfg.observe.activity_interval_ms.max(1000),
            observe_notifications: cfg.capabilities.notifications != ronak_core::model::Access::Off,
        },
    };
    Ok(ronak_gnome::spawn(opts).await?)
}

#[cfg(not(target_os = "linux"))]
async fn gnome(_cfg: &Config) -> Result<AdapterHandle> {
    bail!("the GNOME adapter only exists on Linux")
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn print_status(s: &Value) {
    let yes = |v: &Value| v.as_bool().unwrap_or(false);
    println!(
        "ronakd {} · adapter: {} ({})",
        s["version"].as_str().unwrap_or("?"),
        s["adapter"].as_str().unwrap_or("?"),
        if yes(&s["connected"]) { "connected" } else { "waiting for desktop" }
    );
    if let Some(p) = s["platform"].as_object() {
        println!("desktop: {} {}", p["desktop"].as_str().unwrap_or(""), p["desktop_version"].as_str().unwrap_or(""));
    }
    println!(
        "observing: {} · mode: {} · windows: {}",
        if yes(&s["observing"]) { "yes" } else { "PAUSED" },
        s["mode"].as_str().unwrap_or("?"),
        s["windows"]
    );
    match s["task"].as_object() {
        Some(t) => println!(
            "task: {} ({:.0}% confident, via {})",
            t["label"].as_str().unwrap_or("?"),
            t["confidence"].as_f64().unwrap_or(0.0) * 100.0,
            t["source"].as_str().unwrap_or("?")
        ),
        None => println!("task: not sure yet"),
    }
    if let Some(i) = s["intent"].as_object() {
        let mut parts = vec![];
        if let Some(a) = i["activity"].as_str() {
            parts.push(format!("activity: {a}"));
        }
        if let Some(p) = i["project"].as_str() {
            parts.push(format!("project: {p}"));
        }
        if !parts.is_empty() {
            println!("      {}", parts.join(" · "));
        }
        for e in i["evidence"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            println!("      because {e}");
        }
    }
    if let Some(caps) = s["capabilities"].as_array().filter(|c| !c.is_empty()) {
        let list: Vec<String> = caps
            .iter()
            .map(|c| format!("{} ({})", c["name"].as_str().unwrap_or("?"), c["access"].as_str().unwrap_or("?")))
            .collect();
        println!("capabilities: {}", list.join(", "));
    }
    if let Some(p) = s["panel"].as_object() {
        println!(
            "panel: {} · {} · inside-app reading: {}",
            if yes(&p["visible"]) { "shown" } else { "hidden (Super+Alt+Space)" },
            p["style"].as_str().unwrap_or("?"),
            if yes(&s["elements"]["enabled"]) { "on" } else { "off" }
        );
    }
    match s["suggestion"].as_object() {
        Some(sg) => println!(
            "suggestion {}: {}  (ronakd apply / dismiss)",
            sg["id"].as_str().unwrap_or(""),
            sg["summary"].as_str().unwrap_or("")
        ),
        None => println!("suggestion: none"),
    }
    if let Some(store) = s["store"].as_object() {
        println!(
            "store: {} events, {} feedback entries, {}",
            store["events"],
            store["feedback"],
            if store["encrypted"].as_bool().unwrap_or(false) { "encrypted" } else { "NOT encrypted" }
        );
    }
    if let Some(apps) = s["top_apps_24h"].as_array().filter(|a| !a.is_empty()) {
        let list: Vec<String> =
            apps.iter().map(|a| format!("{} {}m", a["app"].as_str().unwrap_or("?"), a["minutes"])).collect();
        println!("last 24h: {}", list.join(", "));
    }
}

fn print_probe(r: &Value) {
    let p = &r["profile"];
    println!(
        "CPU: {} ({} threads; {})",
        p["cpu"]["model"].as_str().unwrap_or("?"),
        p["cpu"]["logical_cores"],
        p["cpu"]["features"]
            .as_array()
            .map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    );
    println!("Memory: {} GB", p["memory_mb"].as_u64().unwrap_or(0) / 1024);
    let npus = p["npus"].as_array().cloned().unwrap_or_default();
    if npus.is_empty() {
        println!("NPU: none found");
    }
    for n in npus {
        println!(
            "NPU: {} [{}] at {} (driver {})",
            n["family"].as_str().unwrap_or("?"),
            n["pci_id"].as_str().unwrap_or("?"),
            n["device_node"].as_str().unwrap_or("?"),
            n["driver"].as_str().unwrap_or("?")
        );
        if let Some(rt) = n["linux_llm_runtime"].as_str() {
            println!("     LLMs on Linux via: {rt}");
        }
    }
    for g in p["gpus"].as_array().cloned().unwrap_or_default() {
        let vram = g["vram_mb"].as_u64().map(|v| format!(", {v} MB VRAM")).unwrap_or_default();
        println!(
            "GPU: {} ({}{}{})",
            g["vendor"].as_str().unwrap_or("?"),
            g["driver"].as_str().unwrap_or("?"),
            vram,
            if g["integrated"].as_bool().unwrap_or(false) { ", integrated" } else { "" }
        );
    }
    for s in r["servers"].as_array().cloned().unwrap_or_default() {
        println!(
            "Local LLM server: {} at {} ({} models)",
            s["kind"].as_str().unwrap_or("?"),
            s["base_url"].as_str().unwrap_or("?"),
            s["models"].as_array().map(|m| m.len()).unwrap_or(0)
        );
    }
    let plan = &r["plan"];
    println!();
    println!(
        "Tier 1 (always-on classifier): {} via {}",
        plan["classifier"]["device"].as_str().unwrap_or("?").to_uppercase(),
        plan["classifier"]["runtime"].as_str().unwrap_or("?")
    );
    println!("     why: {}", plan["classifier"]["reason"].as_str().unwrap_or(""));
    println!(
        "Tier 2 (occasional LLM): {} via {}",
        plan["llm"]["device"].as_str().unwrap_or("?").to_uppercase(),
        plan["llm"]["runtime"].as_str().unwrap_or("?")
    );
    println!(
        "     why: {}; suggested model size: {}",
        plan["llm"]["reason"].as_str().unwrap_or(""),
        plan["llm_model_hint"].as_str().unwrap_or("")
    );
    for n in plan["notes"].as_array().cloned().unwrap_or_default() {
        println!("note: {}", n.as_str().unwrap_or(""));
    }
}
