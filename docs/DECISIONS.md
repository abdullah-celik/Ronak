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

## v0.2: understanding what happens inside apps (November 2026)

| # | Question | Decision | Why | Revisit when |
| --- | --- | --- | --- | --- |
| 24 | Scope of v0.2 | A thin slice of all three next stages (elements, capabilities, surfaces), each serving intent recognition | Proves the whole path from "inside the app" to "a panel that shows what Ronak understood" before deepening any part | v0.3 planning |
| 25 | Schema change | Additive: schema 0.2 adds fields; 0.1 documents stay valid and readers accept both | Nobody built on 0.1 has to change anything | A change that cannot be additive (then 1.0) |
| 26 | Inside-app signals | AT-SPI (the screen-reader bus): structure plus text; focused element, selected tab, editable fields, terminal tail | Structured, cheap, already exposed by GTK, Qt, Firefox, LibreOffice; no screenshots | Vision fallback for apps without accessibility |
| 27 | Chromium and Electron | They hide their tree until accessibility is switched on; Ronak asks once per app in the panel | Switching it on costs those apps some performance, so it is your call | If they expose trees by default |
| 28 | Detail levels | New level `content` above `full`: text inside apps and notification bodies; password fields are never read at any level | Text is far more sensitive than titles and must be separately grantable | n/a |
| 29 | Content retention | Text is kept 24 hours (encrypted), then condensed into hourly digests (counts and frequent words) and deleted | Long enough to learn from, short enough to limit exposure; digests keep patterns, not sentences | When the classifier shows what features it needs |
| 30 | Intent vocabulary | A fixed taxonomy (task → activity, e.g. coding → debugging) plus your own labels | A shared vocabulary for agents and training data, without forcing you into it | When labels show missing categories |
| 31 | Corrections | "Not right?" switches immediately, holds your label for 10 minutes, and saves it with the features at that moment as a training label | Correction must feel instant; the label is the most valuable training data there is | n/a |
| 32 | Explainability | Every task carries evidence ("Code had 85% of your attention") and alternatives; evidence never quotes app text | You can only correct what you can see; quoting text would leak content into lower detail levels | n/a |
| 33 | Surface format | Ronak's own declarative component catalog (A2UI-shaped: flat list of components with ids, closed set of types and actions), validated in Rust and by `schema/surface.schema.json` | LLMs and agents get freedom to compose, but only from parts that cannot run code or reach the network | Adopting A2UI itself once it is stable |
| 34 | Who composes | Rules composer now, behind a `Composer` trait; an LLM composer comes when the rules version is stable. Any output is validated, invalid output is dropped | A stable baseline first; the trait makes the LLM a drop-in | v0.3 |
| 35 | Drawing surfaces | A GTK 4 + libadwaita app (`ronak-panel`) for panels; small glances in the shell later | Native look, real accessibility, crash isolation from gnome-shell | If glances need to live in the top bar |
| 36 | Panel style | Side panel (docked right, layouts leave its width free) or floating card; switchable from the panel header, remembered | Different screens and habits; you asked to choose | n/a |
| 37 | Panel placement | The extension places the panel window | Wayland apps cannot position themselves | Never on Wayland |
| 38 | Capabilities | Media players (MPRIS), calendar (Evolution Data Server, read-only), notifications (through the extension), MCP servers you configure (Ronak as MCP client) | The most telling context signals that GNOME already exposes | More providers per platform |
| 39 | Capability permissions | Per kind: `off`, `read` (default) or `act`, in the config | Simple allowlist with a clear line between seeing and doing | Per-provider permissions |
| 40 | Notifications | App and title at `full`; body only at `content`; exclusion rules applied inside the shell; nothing leaves the shell when the capability is off; kept in memory for one hour, never stored | Notification text is private messages | n/a |
| 41 | AI agents acting | Agents may *propose* a layout or a panel card over MCP; you accept. `mcp.allow_proposals` turns it off | Agents become useful without being able to change anything by themselves | Per-client permissions |
| 42 | Resources | Lower the CPU weight (cgroup `CPUWeight`) of apps outside the current task; never the focused app, a meeting, or media that is playing; restored when Ronak stops | Helps the foreground task on a busy laptop with no visible side effects | When memory pressure matters too |
| 43 | NPU tier | Deferred to v0.3 | v0.2 is about signals and surfaces; the classifier needs their data first | v0.3 |
| 44 | End-to-end tests | `scripts/headless-test.sh`: a real GNOME Shell without a screen, the extension, the daemon, the panel and a test app | Mocks cannot show whether Mutter, AT-SPI and GTK really behave as assumed (it found a focus race on its first run) | CI once runners can start a headless shell reliably |
