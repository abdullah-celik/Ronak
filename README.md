# Ronak · روناک

*Kurdish for light, clarity.*

Ronak is a **local-first, intent-aware context layer for the desktop**. It
notices what you are working on (coding, writing, a meeting) and arranges
your screen for it, so you never have to drag a window corner again. When it
gets something wrong, one key puts everything back, and it learns from that.

Everything runs on your own machine. Nothing is sent anywhere.

> **Status: early.** The full pipeline works and is tested end to end
> against a simulated desktop and a real D-Bus session. The GNOME extension
> has not yet been run on a real GNOME 46 desktop. That is the next step.

## What it does today

- **Observes** windows, focus, workspaces, monitors and idle time through a
  small GNOME Shell extension, with privacy rules applied before data leaves
  the compositor.
- **Infers your task** from focus history with a transparent heuristic, and
  gives each window a role: primary, reference, monitor or background.
- **Suggests a layout** computed by a deterministic solver, shown in the top
  bar. **Super+Alt+Enter** applies it, **Super+Alt+Z** undoes, **Super+Alt+P**
  pauses. During meetings it also offers to silence notifications.
- **Remembers** in an encrypted local database (SQLCipher, key in GNOME
  Keyring), keeping raw events for 30 days.
- **Finds your AI hardware**: NPUs, GPUs, CPU features and local LLM servers,
  and plans where each model should run (`ronakd probe`).
- **Serves context to AI agents** over the Model Context Protocol, read-only,
  with titles hidden by default (`ronakd mcp`).

## Try it in two minutes, with a simulated desktop

Needs Rust 1.89 or newer ([rustup.rs](https://rustup.rs); Ubuntu's packaged Rust is too old),
plus `sudo apt install build-essential pkg-config perl`.

```bash
git clone https://github.com/abdullah-celik/Ronak.git ronak && cd ronak
export RONAK_DATA_DIR=/tmp/ronak-mock RONAK_RUNTIME_DIR=/tmp/ronak-mock/run RONAK_ENCRYPTION=none
cargo run -p ronakd -- run --platform mock --auto-accept --mock-beat-ms 500
```

In a second terminal (same three `export`s):

```bash
cargo run -q -p ronakd -- status
cargo run -q -p ronakd -- context --detail full
cargo run -q -p ronakd -- probe
```

## Run it on your GNOME desktop

```bash
sudo apt install libglib2.0-bin gnome-shell-extensions
scripts/install-dev.sh          # builds, installs ronakd, links the extension, adds the service
scripts/nested-shell.sh         # try it in a nested GNOME session first: crashes can't hurt you
```

When you are happy with it:

```bash
gnome-extensions enable ronak@ronak.dev    # then log out and back in
systemctl --user enable --now ronakd
ronakd status
```

Logs: `journalctl --user -u ronakd -f` (daemon) and
`journalctl -f -o cat /usr/bin/gnome-shell | grep ronak` (extension).
Uninstall with `scripts/uninstall-dev.sh` (`--purge` also deletes data).

## Use it from an AI agent

Any MCP client can launch `ronakd mcp` as a stdio server. For example, in
Claude Code:

```bash
claude mcp add ronak -- ~/.local/bin/ronakd mcp
```

Tools: `get_current_task`, `get_current_context`, `get_layout_suggestion`,
`get_hardware_profile`. What an agent may see is set in
`~/.config/ronak/config.toml` under `[mcp]`.

## Documentation

- [OS primer](docs/OS-PRIMER.md): Wayland, compositors, extensions, D-Bus,
  systemd, sockets, keyrings and NPUs, explained with commands to try.
- [Architecture](docs/ARCHITECTURE.md): layers, crates, data flow, and how to
  add Windows or another desktop.
- [Privacy and trust](docs/PRIVACY.md): what is recorded, guarantees, limits.
- [Design decisions](docs/DECISIONS.md) and [Roadmap](docs/ROADMAP.md).
- [Context schema](schema/context.schema.json) and
  [adapter protocol](schema/adapter-protocol.schema.json), with
  [examples](schema/examples/).
- [Example configuration](config/ronak.example.toml).

## Repository layout

```text
schema/                 open JSON Schemas (source of truth) + examples
crates/ronak-core       platform-independent model, privacy, intent, layout, actions
crates/ronak-hw         NPU/GPU/CPU detection and inference planning
crates/ronak-store      encrypted SQLite store
crates/ronak-gnome      daemon side of the GNOME adapter (D-Bus)
crates/ronakd           the daemon and CLI (control socket, MCP, LLM tier, mock desktop)
extension/              GNOME Shell extension (GJS) and its tests
scripts/                install, nested session, uninstall
packaging/systemd       user service
docs/                   primer, architecture, privacy, decisions, roadmap
```

## Development

```bash
cargo test --workspace            # Rust: 31 tests, including schema validation
(cd extension && npm test)        # JS: privacy rules checked against the same fixture as Rust
cargo clippy --workspace --all-targets -- -D warnings
```

## License

The daemon, crates and schemas are dual-licensed under
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The
GNOME Shell extension in `extension/ronak@ronak.dev` is licensed
[GPL-2.0-or-later](extension/ronak@ronak.dev/LICENSE), as GNOME requires for
extensions.
