# Roadmap

Each phase ends with something you use daily. Status as of the first commit.

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
- [ ] AT-SPI sensor: focused element role and name, selected tab

## Phase 2: Mover ✅ (needs real-world use)

- [x] Actions: place, focus, minimize, workspaces, Do Not Disturb
- [x] Undo stack, inverse computed before every action
- [x] Deterministic layout solver with stability and drag protection
- [ ] Verify placement on GNOME 46 with Electron, Firefox, terminals, GTK4 apps
- [ ] Multi-monitor layouts (model is ready; solver arranges the active monitor only)
- [ ] Respect per-app minimum sizes reported by the compositor

## Phase 3: Classifier

- [x] Transparent heuristic intent model with task hysteresis
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
- [ ] Preferences panel (GTK4/libadwaita) for exclusions, shortcuts, mode

## Phase 5: Learner

- [ ] Learn per-user layout preferences from overrides (ratios, sides, which apps pair)
- [ ] Learn task labels from corrections
- [ ] Opt-in automatic mode once acceptance is consistently high
- [ ] Restore a whole task's workspace when you return to it

## Phase 6: Vision and beyond

- [ ] Optional vision sensor for apps with no accessibility data, run only when cheap signals are ambiguous
- [ ] MCP actions behind per-client permissions (an agent may *propose* a layout; you accept it)
- [ ] Windows adapter (Win32 + UI Automation), named-pipe control channel
- [ ] KDE (KWin script) and Hyprland (IPC socket) adapters
- [ ] Hardware companion: ESP32 or Pico with a knob and a small display that shows the task and accepts layouts

## Ecosystem goals

- [ ] Publish the context schema as a standalone, versioned spec
- [ ] An open, privacy-preserving benchmark for desktop intent inference (volunteers donate anonymized, labelled sessions)
- [ ] Propose a Wayland protocol for trusted external window management
- [ ] Publish the extension on extensions.gnome.org (needs a domain for the UUID and D-Bus names)
