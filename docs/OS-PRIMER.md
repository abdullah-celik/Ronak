# OS primer: the parts of Linux Ronak touches

This is the background you need to work on Ronak, written for someone who is
comfortable programming but rusty on operating-system internals. Each section
ends with commands you can run on your own machine to see the idea for real.

## 1. User space and kernel space

The **kernel** is the program that owns the hardware: memory, CPU time,
devices. Everything else runs in **user space** as ordinary processes that ask
the kernel for things through *system calls* (open a file, send a packet, map
memory). Ronak is entirely user space. It never needs root.

The kernel exposes much of its internal state as *virtual files*:

- **`/proc`** describes processes and the system. `/proc/<pid>/exe` is a
  symlink to the program a process is running; Ronak uses it to verify who is
  on the other end of a D-Bus connection. `/proc/cpuinfo` and `/proc/meminfo`
  feed the hardware probe.
- **`/sys`** (sysfs) describes devices and drivers. Every GPU appears under
  `/sys/class/drm/cardN`, every NPU under `/sys/class/accel/accelN`, each with
  a `device/vendor` and `device/device` PCI id and a `driver` symlink.

```bash
readlink /proc/$$/exe                      # which shell am I running?
grep -m1 "model name" /proc/cpuinfo
ls -l /sys/class/accel/ 2>/dev/null        # NPUs, if your kernel supports yours
for c in /sys/class/drm/card[0-9]; do echo "$c $(cat $c/device/vendor) $(basename $(readlink $c/device/driver))"; done
```

## 2. How pixels get on screen: display servers and compositors

Apps do not draw directly to the screen. They draw into their own buffers and
hand them to a **display server**, which decides where each window goes and
combines them into the final image. The part that combines (composites) is
the **compositor**.

- **X11** (1984) was a shared display server. Any app could list every
  window, read its title, move it, even read its pixels or keystrokes. That is
  why old automation tools like `xdotool` and `wmctrl` worked, and why X11 is
  considered insecure.
- **Wayland** is a protocol, not a server. The compositor *is* the display
  server. Each app talks only to the compositor and knows only about its own
  windows. It cannot see, move or read anyone else's. That is excellent for
  security and the reason Ronak needs a special arrangement.

GNOME's compositor is **Mutter**, and it runs as a library *inside* the
`gnome-shell` process, together with the top bar, overview and so on. On
Ubuntu 24.04 you have GNOME 46; GNOME 49 and 50 removed the X11 session
entirely. Old X11 apps still run through **Xwayland**, a small X server that
itself is a Wayland client.

```bash
echo $XDG_SESSION_TYPE          # wayland
gnome-shell --version           # GNOME Shell 46.x on Ubuntu 24.04
ps -o pid,cmd -C gnome-shell
```

## 3. GNOME Shell extensions: code inside the compositor

Because only the compositor sees every window, Ronak's "eyes and hands" have
to run inside it. GNOME allows exactly this through **extensions**:
JavaScript files that gnome-shell loads into itself at login, run by **GJS**
(GNOME's JavaScript engine, based on Firefox's SpiderMonkey). Through
**GObject Introspection**, JavaScript can call the C libraries directly:
`gi://Meta` is Mutter, `gi://Shell` is gnome-shell, `gi://Gio` and
`gi://GLib` are GNOME's base libraries.

Two consequences shape Ronak's design:

1. **A bug in an extension can freeze your desktop.** It runs in the same
   process and on the same thread that draws your screen. So the extension
   does as little as possible and all thinking happens in a separate process.
2. **On Wayland you cannot restart gnome-shell without logging out.** Changes
   to extension code take effect at the next login. For development we use a
   **nested session**: a second gnome-shell running inside a window
   (`gnome-shell --nested --wayland` on GNOME 46-48; `--devkit` on 49+).
   `scripts/nested-shell.sh` does all the setup.

Key Mutter objects you will meet in `extension.js`:

| Object | What it is |
| --- | --- |
| `global.display` (`Meta.Display`) | the whole screen setup: windows, focus, monitors |
| `Meta.Window` | one window: `get_title()`, `get_frame_rect()`, `move_resize_frame()`, `minimize()` |
| `global.workspace_manager` | workspaces and their work areas (screen minus panels) |
| `Shell.WindowTracker` | maps a window to the application (`.desktop` file) it belongs to |

```bash
gnome-extensions list --enabled
journalctl -f -o cat /usr/bin/gnome-shell     # extension logs (console.log) land here
```

GNOME Shell also has a built-in inspector: press Alt+F2, type `lg`, press
Enter. Its "Evaluator" tab runs JavaScript inside the shell. Try
`global.display.list_all_windows().map(w => w.get_title())`.

## 4. D-Bus: how desktop programs talk to each other

**D-Bus** is the desktop's message bus. A bus daemon (`dbus-daemon` or
`dbus-broker`) routes messages between connected programs. There are two
buses: the **system bus** (hardware, network, login) and your **session
bus** (one per login, for your apps). Ronak uses only the session bus.

The vocabulary:

- **Unique name**: every connection gets one automatically, like `:1.42`.
- **Well-known name**: a stable name a program can claim, like
  `org.gnome.Shell` or our `dev.ronak.Daemon1`. Only one connection owns it
  at a time.
- **Object path**: an address inside a program, like `/dev/ronak/Daemon1`.
- **Interface**: a named set of methods, properties and signals on an object.
- **Method call**: a request with a reply, sent to one destination.
- **Signal**: a broadcast that anyone can subscribe to.

Ronak's two ends:

```text
ronakd (Rust, zbus)                      gnome-shell + extension (GJS, Gio)
owns dev.ronak.Daemon1                   owns dev.ronak.Shell1
  Hello(s) -> s      <-- method call --    when ronakd appears on the bus
  PushEvents(s)      <-- method call --    window and focus events
                     -- method call -->    Execute(s) -> s: place, focus, DND
```

Events travel as **method calls to one destination**, not as signals,
because signals are broadcast and any program could listen to your window
titles. Each side also verifies the other: it asks the bus for the peer's
process id (`GetConnectionUnixProcessID`) and checks `/proc/<pid>/exe`.

```bash
busctl --user list | grep -i ronak                # who owns our names
busctl --user introspect dev.ronak.Daemon1 /dev/ronak/Daemon1
dbus-monitor --session "destination='dev.ronak.Daemon1'"   # watch traffic live
```

A GUI explorer, **D-Spy**, is in GNOME Software and is the friendliest way
to browse the bus.

## 5. Accessibility: AT-SPI (planned sensor)

Screen readers need to know what is *inside* windows: buttons, text fields,
which tab is selected. Apps publish this over **AT-SPI**, which runs on a
separate accessibility bus (its address comes from `org.a11y.Bus` on the
session bus). It is a rich, structured, privacy-friendlier alternative to
screenshots, and the next sensor on the roadmap. Not every app supports it
well; Electron apps often need a flag.

```bash
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress
```

## 6. Idle time without keylogging

Wayland deliberately does not let apps read global keyboard input. Mutter
keeps an **idle monitor** that knows how long since the last input of any
kind, without saying which input. The extension samples
`global.backend.get_core_idle_monitor().get_idletime()` every 15 seconds.
That is all Ronak knows about your typing: active, idle, or away.

## 7. Background services: systemd user units

**systemd** starts and supervises services. Besides the system instance (pid
1), every logged-in user gets their own **user manager** that runs services
as you, with no root involved. `ronakd.service` is a user unit tied to
`graphical-session.target`: it starts with your desktop, restarts on a crash,
and its stderr goes to the **journal**.

```bash
systemctl --user status ronakd
journalctl --user -u ronakd -f
systemctl --user list-units --type=target | grep graphical
```

## 8. Where files go: XDG directories

The XDG Base Directory convention says where per-user files belong:

| Variable | Default | Ronak keeps |
| --- | --- | --- |
| `XDG_CONFIG_HOME` | `~/.config` | `ronak/config.toml` |
| `XDG_DATA_HOME` | `~/.local/share` | `ronak/ronak.db` (encrypted) |
| `XDG_RUNTIME_DIR` | `/run/user/<uid>` | `ronak/control.sock` |

`XDG_RUNTIME_DIR` is special: a RAM-backed folder only you can access,
created at login and wiped at logout. Perfect for sockets.

## 9. Unix domain sockets

A **Unix socket** is like a network socket that exists only on this machine,
addressed by a file path. Permissions on that path decide who can connect.
`ronakd` listens on `$XDG_RUNTIME_DIR/ronak/control.sock` (directory mode 700,
socket 600) and speaks JSON-RPC, one JSON object per line. The CLI and the MCP
bridge connect to it. Windows has the same idea under the name *named pipes*.

```bash
ls -la $XDG_RUNTIME_DIR/ronak/
echo '{"jsonrpc":"2.0","id":1,"method":"status"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/ronak/control.sock
```

## 10. Secrets: the keyring and encryption at rest

GNOME Keyring implements the **Secret Service** D-Bus API: a store for
passwords that is encrypted with your login password and unlocked when you
log in. Ronak generates a random 256-bit key on first run, stores it there,
and uses it to encrypt its database with **SQLCipher** (SQLite plus AES-256).
Copy `ronak.db` to another machine and it is noise. On Windows the same code
path uses Credential Manager; on macOS, Keychain.

```bash
secret-tool search service ronak     # shows the stored key entry
```

## 11. NPUs and the accel subsystem

An **NPU** (neural processing unit) is a block on recent laptop chips built
for the matrix arithmetic in neural networks, at a fraction of a GPU's power.
Linux added a dedicated **compute accelerator** subsystem: drivers register
devices as `/dev/accel/accelN` and `/sys/class/accel/accelN`.

| Vendor | Kernel driver | Userspace runtime for models |
| --- | --- | --- |
| AMD Ryzen AI (XDNA, XDNA 2) | `amdxdna` (mainline since 6.14) | XRT; FastFlowLM and Lemonade for LLMs (XDNA 2 only) |
| Intel Core Ultra | `intel_vpu` | OpenVINO (CPU, GPU and NPU) |

A model runs on an NPU only if some runtime has a backend for it. That is why
Ronak separates *what* to run from *where*: `ronakd probe` finds your
devices and running servers and picks the best place for each tier.

```bash
ronakd probe
ls -l /dev/accel/ 2>/dev/null
lsmod | grep -E "amdxdna|intel_vpu"
```

## 12. Putting it together

```text
          kernel ── /sys, /proc, /dev/accel ──────────────┐
             │                                            │
   Mutter (in gnome-shell) ── extension.js                │
             │        ▲  D-Bus session bus                │
             ▼        │                                   ▼
          ronakd (systemd user service) ── hardware probe, SQLCipher DB
             │  Unix socket ($XDG_RUNTIME_DIR)            │  keyring (Secret Service)
             ▼                                            ▼
   ronakd CLI · ronakd mcp ── stdio ── AI agents     local LLM server (localhost)
```

Every arrow stays on your machine.
