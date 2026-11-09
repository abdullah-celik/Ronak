# Roadmap

Each phase ends with something you use daily. Status as of v0.2 (November 2026).

## Phase 0: Foundations ✅

- [x] Workspace with core, hardware, store, GNOME adapter and daemon crates
- [x] Open JSON Schemas for context and adapter protocol, with examples
- [x] GNOME Shell extension skeleton, settings schema, keybindings
- [x] Nested development session (`scripts/nested-shell.sh`)
- [x] Hardware probe: NPUs (`/sys/class/accel`), GPUs, CPU features, local LLM servers
- [x] CI: Rust format, lint, tests; extension tests and schema check

## Phase 1: Observer ✅ (needs real-world use)

- [x] Extension reports windows, focus, workspaces, monitors, idle time
- [x] Exclusion rules applied inside the compositor and again in the daemon
- [x] Encrypted event store with 30-day retention and hourly app usage
- [x] `ronakd status`, `ronakd context`
- [ ] Run it for a week on the real desktop; fix what breaks
- [x] AT-SPI sensor: focused element role and name, selected tab (v0.2)

## Phase 2: Mover ✅ (needs real-world use)

- [x] Actions: place, focus, minimize, workspaces, Do Not Disturb
- [x] Undo stack, inverse computed before every action
- [x] Deterministic layout solver with stability and drag protection
- [ ] Verify placement on GNOME 46 with Electron, Firefox, terminals, GTK4 apps
- [ ] Multi-monitor layouts (model is ready; solver arranges the active monitor only)
- [ ] Respect per-app minimum sizes reported by the compositor

## Phase 3: Classifier

- [x] Transparent heuristic intent model with task hysteresis
- [x] Corrections from the panel or `ronakd correct`, saved as training labels (v0.2)
- [ ] Labelling tool: review your stored task spans and correct them
- [ ] Export features from the store; train a small classifier (scikit-learn, then ONNX)
- [ ] Tier-1 ONNX runtime in `ronakd` with execution provider chosen by `ronak-hw::plan`
  (OpenVINO for Intel NPU, Vitis AI for AMD XDNA 2, CPU otherwise)
- [ ] Compare heuristic vs classifier: accuracy, latency, power

## Phase 4: Suggester ✅ (first version)

- [x] Suggestions shown in the top bar, accepted with Super+Alt+Enter
- [x] Feedback recorded: shown, accepted, dismissed, undone, overridden
- [x] Tier-2 LLM through any local OpenAI-compatible server, loopback only
- [ ] A subtle on-screen preview of the suggested layout before accepting
- [x] The Ronak panel (GTK4/libadwaita): intent, evidence, corrections, suggestion, context (v0.2)
- [ ] Preferences in the panel for exclusions, shortcuts, mode, capabilities

## v0.2: Understanding and surfaces ✅ (needs real-world use)

- [x] Schema 0.2 (additive): elements, capabilities, finer intent (activity, project, evidence, alternatives)
- [x] Detail level `content`; text kept 24 hours, then condensed into pattern digests
- [x] AT-SPI reader with node, depth, text and time budgets; password fields never read
- [x] Ask once per app before switching on accessibility for Chromium and Electron
- [x] Activity and project detection from terminal output, titles, calendar, media
- [x] Capabilities: MPRIS media, Evolution calendar (with recurrence), notifications, MCP servers as a client
- [x] Surface catalog with a validator and JSON Schema; rules composer; agent proposals
- [x] `ronak-panel`: side panel or floating card, Super+Alt+Space, placed by the extension
- [x] Lower CPU weight for apps outside the task
- [x] Headless GNOME Shell end-to-end test
- [ ] Use it daily on the laptop; tune markers and evidence weights
- [ ] Context glances in the top bar (shell-drawn)
- [ ] An LLM composer for surfaces, held to the same validator

## v0.3 (planned)

- [ ] Tier-1 classifier trained on v0.2 labels and digests, on the NPU when available
- [ ] LLM surface composer
- [ ] Per-client and per-provider permissions

## Phase 5: Learner

- [ ] Learn per-user layout preferences from overrides (ratios, sides, which apps pair)
- [ ] Learn task labels from corrections
- [ ] Opt-in automatic mode once acceptance is consistently high
- [ ] Restore a whole task's workspace when you return to it

## Phase 6: Vision and beyond

- [ ] Optional vision sensor for apps with no accessibility data, run only when cheap signals are ambiguous
- [x] Agents may *propose* a layout or a panel card over MCP; you accept it (v0.2)
- [ ] Per-client permissions for agents
- [ ] Windows adapter (Win32 + UI Automation), named-pipe control channel
- [ ] KDE (KWin script) and Hyprland (IPC socket) adapters
- [ ] Hardware companion: ESP32 or Pico with a knob and a small display that shows the task and accepts layouts

## Ecosystem goals

- [ ] Publish the context schema as a standalone, versioned spec
- [ ] An open, privacy-preserving benchmark for desktop intent inference (volunteers donate anonymized, labelled sessions)
- [ ] Propose a Wayland protocol for trusted external window management
- [ ] Publish the extension on extensions.gnome.org (needs a domain for the UUID and D-Bus names)
