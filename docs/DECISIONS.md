# Design decisions

Every significant choice, why it was made, and what would make us revisit
it. Recorded at the project's start (November 2026); add new entries at the
bottom rather than rewriting old ones.

| # | Question | Decision | Why | Revisit when |
| --- | --- | --- | --- | --- |
| 1 | First platform | GNOME 46 on Ubuntu 24.04 (Omakub), extension declares 46-48 | The author's daily desktop: you only get feedback from something you use | GNOME 49+ testing: the maximize API changed |
| 2 | Generality | Platform-independent core; platforms are adapters behind channels | Windows, macOS, KDE and Hyprland should need only a new adapter | Never: this is the founding rule |
| 3 | Daemon language | Rust from the start | Long-lived, low-overhead system daemon; good D-Bus (zbus), SQLite and ONNX bindings; builds on Windows | n/a |
| 4 | Where compute runs | Detect hardware; prefer NPU for always-on work; run on whatever exists | NPUs are spreading and are the most power-efficient, but Linux support is uneven (XDNA 2 and Intel NPU only) | As Linux NPU runtimes mature |
| 5 | Inference structure | Two tiers: tiny always-on classifier, occasional local LLM via OpenAI-compatible server | Power and latency budgets differ by orders of magnitude; any server works (Ollama, Lemonade, llama.cpp) | If one embedded runtime covers LLMs on all NPUs |
| 6 | Extension ↔ daemon IPC | D-Bus on GNOME, hidden inside the GNOME adapter; JSON payloads | D-Bus is the only native way into gnome-shell; JSON keeps one schema | n/a (other adapters choose their own transport) |
| 7 | Schema source of truth | Standalone JSON Schema in `schema/`; Rust tested against it | Language-neutral contract others can build on | n/a |
| 8 | MCP | Day one, read-only; actions later behind permissions | Makes Ronak useful to any AI agent immediately without new risk | When per-action permissions exist |
| 9 | Signals collected | Window metadata, activity rhythm; AT-SPI next; no screenshots | Cheap, structured, privacy-friendlier than vision | Vision as an opt-in fallback later |
| 10 | Retention | Raw events 30 days; aggregates and feedback kept | Enough for training, bounded exposure | n/a |
| 11 | Privacy protections | Exclusion list (inside the compositor) and encrypted database | Highest-value protections; pause and undo added anyway | n/a |
| 12 | MCP default detail | Task plus app names, no titles | Useful to agents, keeps the most sensitive field private | Per-client overrides already exist |
| 13 | Layouts | Computed, not presets: AI assigns roles and weights, a deterministic solver assigns rectangles | Generality of computed layouts with the predictability of rules | If a learned layout model proves stable |
| 14 | Monitors | Laptop screen first; model and solver are multi-monitor aware | The author's setup; avoid a rewrite later | Multi-monitor layouts are on the roadmap |
| 15 | Automation level | Suggest with one-key accept; opt-in auto mode | Research and experience: unexpected movement destroys trust | When acceptance rates are high |
| 16 | Scope of actions | Placement, focus, minimize, workspaces, Do Not Disturb, all reversible | "Make the user's life easier" without irreversible actions | New actions must define an inverse |
| 17 | License | Core and schema MIT OR Apache-2.0; extension GPL-2.0-or-later | Maximum adoption; GNOME requires GPL-compatible terms for extensions | n/a |
| 18 | Name | Ronak (روناک, Kurdish: light, clarity) | Bringing clarity to the screen | n/a |
| 19 | D-Bus names | `dev.ronak.Daemon1`, `dev.ronak.Shell1`, extension `ronak@ronak.dev` | Reverse-DNS convention; the `1` allows a future incompatible `2` | Rename to a domain the project actually owns before publishing the extension |
| 20 | Storage | SQLite with SQLCipher; key in the OS keyring via the `keyring` crate | One file, transactional, encrypted; same code on Linux, Windows, macOS | n/a |
| 21 | Control interface | Unix socket with JSON-RPC (named pipe on Windows later) | Same-user only, no network, trivial to script | n/a |
| 22 | Re-inference | On change (max once per second) and at least every 10 seconds | Staying in one window for minutes is evidence too (found by the daemon tests) | n/a |
| 23 | Closed windows | Focus history of closed windows counts 25% | When a meeting window closes, the meeting is over | When a learned model replaces the heuristic |
