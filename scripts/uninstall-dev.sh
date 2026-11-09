#!/usr/bin/env bash
# Removes what install-dev.sh installed. Your data stays unless you pass --purge.
set -euo pipefail
UUID="ronak@ronak.dev"

systemctl --user disable --now ronakd 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/ronakd.service"
systemctl --user daemon-reload || true
gnome-extensions disable "$UUID" 2>/dev/null || true
rm -f "$HOME/.local/share/gnome-shell/extensions/$UUID"
rm -f "$HOME/.local/bin/ronakd"

if [ "${1:-}" = "--purge" ]; then
    rm -rf "$HOME/.local/share/ronak" "$HOME/.local/share/ronak-dev" "$HOME/.config/dconf/ronak-dev"
    # The database key lives in your keyring as "ronak / database-key".
    secret-tool clear service ronak username database-key 2>/dev/null || true
    echo "Removed Ronak and all its data."
else
    echo "Removed Ronak. Data kept in ~/.local/share/ronak (run with --purge to delete it)."
fi
