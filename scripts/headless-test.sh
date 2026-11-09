#!/usr/bin/env bash
# End-to-end test with a real GNOME Shell that draws to no screen.
#
# Starts, inside a private D-Bus session and a throwaway home folder:
#   gnome-shell --headless (a virtual 1920x1200 monitor) with the extension,
#   ronakd (which starts ronak-panel), a small GTK test app,
# then checks what the daemon sees: windows with process ids, text read
# inside the app through the accessibility bus, a notification, the panel
# placed by the extension. Saves a screenshot and the JSON it checked.
#
# Nothing touches your real session. Needs gnome-shell, python3 with GTK 4
# bindings (python3-gi, gir1.2-gtk-4.0) and jq.
#
#   scripts/headless-test.sh            results in a temporary folder
#   RONAK_TEST_OUT=out scripts/headless-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UUID="ronak@ronak.dev"
PY="${PYTHON:-python3}"
command -v "$PY" >/dev/null || PY=/usr/bin/python3
OUT="${RONAK_TEST_OUT:-$(mktemp -d)}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

echo "==> Building ronakd and ronak-panel"
cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p ronakd -p ronak-panel
BIN="$ROOT/target/debug/ronakd"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home"
export XDG_RUNTIME_DIR="$WORK/run"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share" XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME/gnome-shell/extensions" "$XDG_CONFIG_HOME/ronak"
chmod 700 "$XDG_RUNTIME_DIR"
cp -r "$ROOT/extension/$UUID" "$XDG_DATA_HOME/gnome-shell/extensions/"
# Desktop entries let GNOME match windows to apps by their app id.
mkdir -p "$XDG_DATA_HOME/applications"
cp "$ROOT/packaging/dev.ronak.Panel.desktop" "$XDG_DATA_HOME/applications/"
cat > "$XDG_DATA_HOME/applications/dev.ronak.TestTextEditor.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Scratch Editor
Exec=true
NoDisplay=true
EOF
glib-compile-schemas "$XDG_DATA_HOME/gnome-shell/extensions/$UUID/schemas"

export RONAK_ENCRYPTION=none
export RONAK_DATA_DIR="$WORK/data"
export RONAK_RUNTIME_DIR="$WORK/ronak"
export WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland
unset DISPLAY
export ROOT UUID BIN OUT PY
cat > "$XDG_CONFIG_HOME/ronak/config.toml" <<'EOF'
[surfaces]
start_visible = true
# Exclusion rules apply to notifications too: this one (shown because the
# test may run as root) must never reach the daemon.
[[observe.exclusions]]
title_contains = "privileged user"
mode = "ignore"
EOF

echo "==> Starting headless GNOME Shell, ronakd and a test app (results: $OUT)"
dbus-run-session -- bash -euo pipefail -c '
    log() { echo "    $*"; }
    gsettings set org.gnome.shell disable-user-extensions false
    gsettings set org.gnome.shell enabled-extensions "[\"$UUID\"]"
    gsettings set org.gnome.desktop.interface toolkit-accessibility true
    # Keep banners off the screenshots; notifications still reach the tray (and Ronak).
    gsettings set org.gnome.desktop.notifications show-banners false
    gsettings --schemadir "$XDG_DATA_HOME/gnome-shell/extensions/$UUID/schemas" \
        set org.gnome.shell.extensions.ronak trusted-daemon-paths "[\"$BIN\"]"

    gnome-shell --headless --wayland --no-x11 --virtual-monitor 1920x1200 >"$OUT/gnome-shell.log" 2>&1 &
    SHELL_PID=$!
    for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; sleep 0.1; done
    [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] || { echo "gnome-shell did not start"; tail -20 "$OUT/gnome-shell.log"; exit 1; }
    log "gnome-shell is up"

    RUST_LOG=info "$BIN" run --platform gnome >"$OUT/ronakd.log" 2>&1 &
    DAEMON_PID=$!
    trap "kill \$DAEMON_PID \$SHELL_PID \${APP_PID:-} 2>/dev/null || true" EXIT
    for _ in $(seq 100); do
        "$BIN" status --json 2>/dev/null | jq -e .connected >/dev/null 2>&1 && break
        sleep 0.2
    done
    "$BIN" status --json | jq -e .connected >/dev/null || { echo "ronakd did not connect"; tail -20 "$OUT/ronakd.log"; exit 1; }
    log "ronakd is connected to the extension"

    "$PY" "$ROOT/scripts/testapp.py" >"$OUT/testapp.log" 2>&1 &
    APP_PID=$!
    sleep 3
    # A headless shell starts in the overview, where nothing has focus.
    # Focusing the app through Ronak leaves it, like clicking the window would.
    WIN=$("$BIN" context --detail full | jq -r "[.windows[] | select(.app.id == \"dev.ronak.TestTextEditor\")][0].id")
    for _ in $(seq 20); do
        "$BIN" rpc surface.action "{\"action\": {\"name\": \"focus_window\", \"args\": {\"window_id\": \"$WIN\"}}}" >/dev/null
        sleep 0.5
        [ "$("$BIN" context --detail full | jq -r .focused_window)" = "$WIN" ] && break
    done
    log "focused the test app (window $WIN)"
    sleep 25   # long enough for the focus evidence to build up
    gdbus call --session --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify "Ronak test" 0 "" "Build finished" "All 42 tests passed" \
        "[]" "{}" 5000 >/dev/null
    "$BIN" panel show >/dev/null
    sleep 4

    "$BIN" status --json >"$OUT/status.json"
    "$BIN" context --detail content >"$OUT/context.json"
    "$PY" "$ROOT/scripts/screenshot.py" "$OUT/screen.png" >/dev/null || log "(no screenshot)"

    # Accept the suggested layout: the editor should leave room for the panel.
    "$BIN" apply >/dev/null || log "(no suggestion to apply)"
    sleep 2
    "$BIN" context --detail full >"$OUT/after-apply.json"
    "$PY" "$ROOT/scripts/screenshot.py" "$OUT/screen-applied.png" >/dev/null || true

    # The other panel style.
    "$BIN" rpc panel.set "{\"style\": \"floating_card\"}" >/dev/null
    sleep 2
    "$BIN" status --json >"$OUT/status-floating.json"
    "$PY" "$ROOT/scripts/screenshot.py" "$OUT/screen-floating.png" >/dev/null || true
'

echo "==> Checking"
fail=0
check() {
    if jq -e "$2" "$OUT/$3" >/dev/null; then echo "    ok   $1"; else echo "    FAIL $1"; fail=1; fi
}
check "the test app is observed"                '.windows | map(select(.app.id == "dev.ronak.TestTextEditor")) | length == 1' context.json
check "windows carry their process id"           '[.windows[] | select(.pid != null)] | length >= 1' context.json
check "the panel itself is never observed"       '[.windows[] | select(.app.id | test("ronak.Panel"; "i"))] | length == 0' context.json
check "the test app has focus"                   '.focused_window != null' context.json
check "coding is inferred, with its project"     '.task.label == "coding" and .task.project == "ronak"' context.json
check "the evidence is explained"                '[.task.evidence[] | select(.signal == "focus_time")] | length >= 1' context.json
check "text inside the app is read (AT-SPI)"     '[(.elements // [])[] | select(.text != null and (.text | test("fn main")))] | length >= 1' context.json
check "the notification arrived with its body"   '[.capabilities[] | select(.kind == "notifications") | .notifications.recent[] | select(.body == "All 42 tests passed")] | length == 1' context.json
check "excluded notifications never arrive"      '[.capabilities[] | select(.kind == "notifications") | .notifications.recent[] | select(.title // "" | test("privileged"))] | length == 0' context.json
check "the panel is shown and subscribed"        '.panel.visible == true and .panel.subscribers >= 1' status.json
check "an accepted layout leaves room for it"    '[.windows[] | select(.app.id == "dev.ronak.TestTextEditor") | .rect | select(.x + .w <= 1920 - 360 and .w > 900)] | length == 1' after-apply.json
check "the panel style can be switched"          '.panel.style == "floating_card"' status-floating.json
[ "$fail" = 0 ] && echo "==> All checks passed. Screenshots: $OUT/screen*.png" || { echo "==> Some checks failed; logs in $OUT"; exit 1; }
