# Privacy and trust

Ronak watches your desktop, so it has to deserve trust. These are the
guarantees, how they are enforced, and where their limits are.

## What Ronak records

| Recorded | Not recorded |
| --- | --- |
| Which app each window belongs to, its title, position, size, workspace, process id | Screenshots (a possible future vision sensor would be opt-in) |
| Which window has focus and for how long | Keystrokes, clipboard |
| Idle time: active, idle (1+ min), away (5+ min) | Which keys or buttons you press |
| Suggestions shown, accepted, dismissed, undone, overridden | Anything from excluded windows beyond what their rule allows |
| Inside the focused app (v0.2): the structure (roles, names, which tab is selected) and some text: the end of a terminal, the field or document you are working in | Password fields, at any setting; apps you excluded; anything while paused |
| Text read inside apps: kept 24 hours, then condensed into hourly digests (counts and frequent words) and deleted | Sentences older than a day |
| Corrections you make, with the signals at that moment, as training labels | Notification text (kept in memory for an hour, never stored) |
| Media playing, calendar events of today, notifications (in memory) | Calendar contents beyond today; contacts |

## Detail levels

Everything that leaves the daemon (to the CLI, to AI agents over MCP, to the
local LLM tier) passes through `privacy::project`, which keeps only what the
chosen level allows:

| Level | Adds |
| --- | --- |
| `task_only` | The task, its activity, confidence and whether you are active |
| `task_and_apps` | Open apps and their roles; capability names; notification counts and senders; evidence that names apps (default for agents) |
| `full` | Window titles, the project name, the structure inside the focused app, media titles, calendar event names, notification titles |
| `content` | Text read inside apps and notification bodies |

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
9. **Password fields are never read.** The accessibility reader skips the
   `password_text` role entirely, including its name, and never descends
   into it. Excluded windows (password managers, private browsing) are not
   read at all.
10. **Text needs its own permission.** App text and notification bodies only
    appear at the `content` level, which nothing gets by default except the
    daemon's own store. Evidence shown to you or to agents names apps, never
    quotes their text.
11. **Asking before changing your system.** Chromium and Electron apps hide
    their insides until the session-wide accessibility switch is on. Ronak
    asks once per app in its panel and remembers your answer.
12. **Agents propose, you decide.** An agent may propose a layout or a panel
    card. Cards are validated against a closed component catalog (no code, no
    images, no links, only a fixed set of actions), labelled with the agent's
    name, and stay until you keep or dismiss them. Set
    `mcp.allow_proposals = false` to turn this off.
13. **Seeing is not doing.** Each capability is `off`, `read` (default) or
    `act`. Media buttons and MCP tool calls need `act`. With notifications
    `off`, the extension stops forwarding them at the source.

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
- **Accessibility is a powerful interface.** Any program running as you
  can read the accessibility bus, just as Ronak does; switching it on for
  Chromium and Electron makes their contents readable to those programs
  too. That is why Ronak asks first.
- **Digests keep frequent words.** A word that appears in what you read or
  typed at least twice in an hour can end up in that hour's digest. Use
  exclusion rules for apps whose words you do not want remembered, or set
  `elements.read_content = false` to keep only structure.
- **The nested development session stores data unencrypted** in
  `~/.local/share/ronak-dev`, because no keyring runs inside it.

## Removing everything

```bash
scripts/uninstall-dev.sh --purge
```

This deletes the binaries, extension link, service, both databases and the
keyring entry. `ronakd patterns` shows what the digests contain.
