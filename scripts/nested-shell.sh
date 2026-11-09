#!/usr/bin/env bash
# Runs a second GNOME Shell inside a window, with its own D-Bus session bus,
# its own settings database and a development ronakd. Nothing here touches
# your real session: if the extension crashes the nested shell, just close
# the window. Ctrl+C in this terminal stops everything.
#
# Environment knobs:
#   RONAK_NESTED_SIZE=1920x1200   size of the nested screen (default 1600x1000)
#   RUST_LOG=debug                more daemon logging
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UUID="ronak@ronak.dev"
EXT_SRC="$ROOT/extension/$UUID"
EXT_LINK="$HOME/.local/share/gnome-shell/extensions/$UUID"
DEV="$HOME/.local/share/ronak-dev"

echo "==> Building ronakd (debug)"
cargo build --manifest-path "$ROOT/Cargo.toml" -p ronakd
BIN="$ROOT/target/debug/ronakd"

# The shell discovers extensions in this folder. A link alone enables nothing.
mkdir -p "$(dirname "$EXT_LINK")" "$DEV"
[ -L "$EXT_LINK" ] || ln -sfn "$EXT_SRC" "$EXT_LINK"
glib-compile-schemas "$EXT_SRC/schemas"

# A separate dconf database, so enabling the extension here does not enable
# it in your real session.
printf 'user-db:ronak-dev\n' > "$DEV/dconf-profile"

MAJOR="$(gnome-shell --version | awk '{print int($3)}')"
if [ "$MAJOR" -ge 49 ]; then
    NESTED_FLAGS="--devkit --wayland"   # GNOME 49+ replaced --nested with the Mutter devkit
else
    NESTED_FLAGS="--nested --wayland"
fi

export ROOT UUID EXT_SRC BIN NESTED_FLAGS
export DCONF_PROFILE="$DEV/dconf-profile"
export RONAK_ENCRYPTION=none               # no GNOME Keyring inside the nested bus
export RONAK_DATA_DIR="$DEV/data"
export RONAK_RUNTIME_DIR="$DEV/run"
export MUTTER_DEBUG_DUMMY_MODE_SPECS="${RONAK_NESTED_SIZE:-1600x1000}"

echo "==> Starting nested GNOME Shell $MAJOR ($NESTED_FLAGS) with a dev ronakd"
echo "    In another terminal:  RONAK_RUNTIME_DIR=$DEV/run $BIN status"
dbus-run-session -- bash -euc '
    gsettings set org.gnome.shell disable-user-extensions false
    gsettings set org.gnome.shell enabled-extensions "[\"$UUID\"]"
    gsettings --schemadir "$EXT_SRC/schemas" set org.gnome.shell.extensions.ronak trusted-daemon-paths "[\"$BIN\"]"
    "$BIN" run --platform gnome &
    DAEMON=$!
    trap "kill $DAEMON 2>/dev/null || true" EXIT
    gnome-shell $NESTED_FLAGS || true
'
