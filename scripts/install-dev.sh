#!/usr/bin/env bash
# Builds ronakd, installs it to ~/.local/bin, links the GNOME Shell extension
# into ~/.local/share/gnome-shell/extensions, compiles its settings schema and
# installs the systemd user service. Enables nothing: you choose when.
# Safe to re-run after every change.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UUID="ronak@ronak.dev"
EXT_LINK="$HOME/.local/share/gnome-shell/extensions/$UUID"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"

echo "==> Building ronakd (release)"
cargo build --release --manifest-path "$ROOT/Cargo.toml" -p ronakd
install -Dm755 "$ROOT/target/release/ronakd" "$BIN_DIR/ronakd"

echo "==> Linking the GNOME Shell extension"
mkdir -p "$(dirname "$EXT_LINK")"
if [ -e "$EXT_LINK" ] && [ ! -L "$EXT_LINK" ]; then
    echo "error: $EXT_LINK exists and is not a symlink; move it away first" >&2
    exit 1
fi
ln -sfn "$ROOT/extension/$UUID" "$EXT_LINK"
glib-compile-schemas "$ROOT/extension/$UUID/schemas"

echo "==> Installing the systemd user service"
install -Dm644 "$ROOT/packaging/systemd/ronakd.service" "$UNIT_DIR/ronakd.service"
systemctl --user daemon-reload

cat <<NEXT

Installed. Nothing is running yet.

  1. Try it in a throwaway nested GNOME session first:
       scripts/nested-shell.sh
  2. When you are happy, enable it in your real session:
       gnome-extensions enable $UUID   # then log out and back in (Wayland cannot restart the shell)
       systemctl --user enable --now ronakd
  3. Watch it:
       ronakd status
       journalctl --user -u ronakd -f
       journalctl -f -o cat /usr/bin/gnome-shell | grep ronak
NEXT
