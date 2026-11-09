# Architecture

Ronak is built as an **open context layer for the desktop**, with an
intent-aware window manager as its first application. The design goal: the
part that understands *what you are doing* must not know which operating
system it runs on, so it can grow into Windows, macOS and other Linux
desktops, and so other tools can build on it.

## Four layers

```text
 ┌──────────────── platform-specific ────────────────┐   ┌──────── platform-independent (ronak-core) ────────┐
 │                                                    │   │                                                    │
 │  SENSORS                                           │   │  STATE           desktop model + focus history     │
 │   GNOME: extension in gnome-shell (JS, D-Bus)      │──▶│  PRIVACY         exclusion rules, detail levels    │
 │   mock:  scripted desktop (Rust)                   │   │  INTENT          task + window roles               │
 │   later: Windows (Win32 + UI Automation), KDE, ... │   │  LAYOUT SOLVER   roles -> rectangles               │
 │                                                    │   │  ACTIONS         reversible changes + undo         │
 │  ACTUATORS                                         │◀──│  SUGGEST         layout + policy -> one suggestion │
 │   same adapter, opposite direction                 │   │                                                    │
 └────────────────────────────────────────────────────┘   └────────────────────────────────────────────────────┘
                                                                     │
                                       ronakd: daemon loop, storage, control socket, MCP, LLM tier
```

The rule that holds it together: **only adapters know about the platform.**
Everything they report is translated into the types in
`ronak-core::model` and `ronak-core::protocol`, which mirror the JSON
Schemas in `schema/`.

## Crates and folders

| Path | Language | Role |
| --- | --- | --- |
| `schema/` | JSON Schema | The open contracts: the context snapshot and the adapter protocol. Source of truth. |
| `crates/ronak-core` | Rust | Model, state, privacy, intent, layout, actions, suggestions, config. No I/O, no OS calls. |
| `crates/ronak-hw` | Rust | Finds NPUs, GPUs, CPU features; plans where each inference tier runs. |
| `crates/ronak-store` | Rust | SQLCipher database: events, hourly aggregates, task spans, feedback. Retention. |
| `crates/ronak-gnome` | Rust | Daemon side of the GNOME adapter: D-Bus service, peer verification. |
| `crates/ronakd` | Rust | The daemon binary and CLI: main loop, control socket, MCP server, LLM tier, mock desktop. |
| `extension/ronak@ronak.dev` | GJS | The GNOME Shell extension: eyes and hands inside the compositor. GPL-2.0-or-later. |
| `tests/fixtures` | JSON | Fixtures shared by Rust and JavaScript tests. |

## Data flow, one event at a time

1. You click on your terminal. Mutter changes focus; the extension's
   `notify::focus-window` handler queues `{"type":"focus_changed","window_id":"…"}`.
2. Every 250 ms the extension flushes its queue with one D-Bus call,
   `dev.ronak.Daemon1.PushEvents`, to the verified `ronakd` connection.
3. `ronak-gnome` checks the sender, parses the JSON into `PlatformEvent`s and
   sends them down a channel.
4. The daemon loop re-applies exclusion rules, folds the event into
   `DesktopState`, records it in the encrypted store, and marks the intent
   as dirty.
5. On the next tick (at most once per second, and at least every 10 seconds)
   `HeuristicIntent` scores tasks from decayed focus time and assigns each
   visible window a role and weight. `TaskTracker` applies hysteresis so a
   glance at Slack does not flip "coding" to "communication".
6. When the task starts, changes or is refined, `suggest::build` asks the
   layout solver for rectangles and adds policy actions (Do Not Disturb in
   meetings). If the result differs from what is on screen, the daemon sends
   `show_suggestion` to the extension, which lights up the top-bar indicator.
7. You press Super+Alt+Enter. The extension sends a `user` event; the daemon
   records the inverse of every action on the undo stack *first*, then sends
   `execute`, and the extension moves the windows.
8. If you drag a window Ronak just placed, the daemon records an
   `overridden` feedback entry: training data for learning your preferences.

## Why each piece is the way it is

**One task owns all state (actor pattern).** Adapter events, control-socket
requests, LLM answers and timer ticks all arrive as messages to a single
`tokio::select!` loop. No locks, no data races, and the order of events is
the order of processing.

**Adapters return channels, not trait objects.** An adapter is "a task that
sends `PlatformEvent`s and receives `AdapterRequest`s". This works the same
for an in-process Rust adapter (mock, future Windows) and an out-of-process
one (the GNOME extension behind D-Bus).

**AI decides roles; a solver decides pixels.** Letting a model emit
coordinates makes windows jump around unpredictably. The solver is
deterministic, keeps the layout stable unless something moves by more than
24 px, never fights a window being dragged, and demotes windows rather than
making tiles smaller than 480×300. Weights are rounded to quarter steps so
small shifts in attention do not reshuffle the screen.

**Every action is reversible before it runs.** `UndoStack::record` computes
inverse actions from the current state before execution.

**JSON over D-Bus instead of typed D-Bus signatures.** D-Bus has its own
type system, but then the schema would exist twice. Passing JSON strings
that validate against `schema/adapter-protocol.schema.json` keeps one source
of truth, and the same messages work over any transport.

**Two inference tiers.** Tier 1 is small, always on, and power-sensitive:
today the transparent heuristic, later a tiny ONNX classifier placed on the
NPU when one is usable. Tier 2 is a local LLM behind any OpenAI-compatible
server, asked only when tier 1 is unsure and at most once a minute.
`ronak-hw::plan` decides where each tier should run.

## The control socket and MCP

`ronakd` listens on `$XDG_RUNTIME_DIR/ronak/control.sock` for JSON-RPC:
`status`, `context.get`, `suggestion.get|apply|dismiss`, `undo`,
`observe.set`, `hardware.get`. The CLI subcommands are thin clients of it.

`ronakd mcp` is a separate process that an AI client launches. It speaks the
Model Context Protocol on stdio and forwards to the control socket. Version 1
exposes four read-only tools (`get_current_task`, `get_current_context`,
`get_layout_suggestion`, `get_hardware_profile`) and two resources (the
current context and the schema). The detail level is chosen per client in
`[mcp]` and enforced by `privacy::project`.

## Adding a platform

Windows is the reference example of how this is meant to grow:

1. Create `crates/ronak-windows` exposing `spawn(opts) -> AdapterHandle`.
2. Sensors: `SetWinEventHook` for focus, create, destroy and move events;
   `EnumWindows`, `GetWindowText`, `GetWindowRect`, `MonitorFromWindow` for
   snapshots; `GetLastInputInfo` for idle time; UI Automation for the
   accessibility sensor.
3. Actuators: `SetWindowPos`, `ShowWindow`, `SetForegroundWindow`; Focus
   Assist for Do Not Disturb.
4. Unlike Wayland, Windows lets a normal process do all of this, so no
   separate plug-in is needed: the adapter is plain Rust inside `ronakd`.
5. In `ronakd`, replace the Unix socket with a named pipe (same JSON-RPC).
   The keyring and SQLCipher code already work on Windows.

A KDE adapter would be a KWin script (JavaScript) speaking the same JSON
protocol over D-Bus; Hyprland's adapter could be pure Rust over its IPC
socket.

## Trust model

See [PRIVACY.md](PRIVACY.md).
