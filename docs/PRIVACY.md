# Privacy and trust

Ronak watches your desktop, so it has to deserve trust. These are the
guarantees, how they are enforced, and where their limits are.

## What Ronak records

| Recorded | Not recorded |
| --- | --- |
| Which app each window belongs to, its title, position, size, workspace | Window contents or screenshots (a possible future vision sensor would be opt-in) |
| Which window has focus and for how long | Keystrokes, clipboard, what you type |
| Idle time: active, idle (1+ min), away (5+ min) | Which keys or buttons you press |
| Suggestions shown, accepted, dismissed, undone, overridden | Anything from excluded windows beyond what their rule allows |

## Guarantees

1. **Local only.** Nothing leaves the machine. The extension talks only to
   `ronakd` over the session bus; `ronakd` listens only on a Unix socket in
   your private runtime directory. The optional LLM tier refuses non-loopback
   endpoints unless you set `allow_remote = true`.
2. **Exclusions apply before data leaves the compositor.** Rules are
   evaluated inside gnome-shell; a redacted window's title never crosses
   D-Bus. The daemon applies the same rules again. Both implementations are
   tested against one shared fixture so they cannot drift apart.
3. **Encrypted at rest.** The database is SQLCipher (AES-256) with a random
   key in GNOME Keyring. The tests check that titles do not appear in the file.
4. **Bounded retention.** Raw events are deleted after 30 days. Hourly app
   usage totals, task spans and feedback are kept; they contain no titles.
5. **Consumers see a reduced view.** AI agents over MCP get `task_and_apps` by
   default: the task and app names, never window titles. You can lower it to
   `task_only` or raise it per client.
6. **Verified peers.** `ronakd` accepts messages only from processes running
   `/usr/bin/gnome-shell`; the extension talks only to a `ronakd` whose
   executable is in its trusted list. Events are sent as direct method calls,
   never broadcast signals.
7. **You can pause.** Super+Alt+P, the indicator switch, or `ronakd pause`.
   While paused the extension sends nothing.
8. **Every change is undoable.** Super+Alt+Z or `ronakd undo`.

## Limits, honestly

- **Same-user processes are trusted by the operating system.** Any
  unsandboxed program running as you can already read your files, including
  Ronak's config, and talk to your session bus. Peer verification protects
  against sandboxed (Flatpak) apps and accidental leaks, not against malware
  already running as you. This is true of every desktop program.
- **The database key is as safe as your keyring.** If your keyring is
  unlocked, a program running as you can ask for the key.
- **Titles are sensitive.** They can contain document names, email subjects
  and chat names. Use exclusion rules generously, or set `observe.titles =
  false` (the intent engine then relies on app identity only).
- **The nested development session stores data unencrypted** in
  `~/.local/share/ronak-dev`, because no keyring runs inside it.

## Removing everything

```bash
scripts/uninstall-dev.sh --purge
```

This deletes the binary, extension link, service, both databases and the
keyring entry.
