// SPDX-License-Identifier: GPL-2.0-or-later
//
// Names and interfaces shared with ronakd (crates/ronak-gnome/src/lib.rs).
// Message payloads follow schema/adapter-protocol.schema.json.

export const PROTOCOL_VERSION = 1;

// The daemon owns this name; the extension calls into it.
export const DAEMON_NAME = 'dev.ronak.Daemon1';
export const DAEMON_PATH = '/dev/ronak/Daemon1';
export const DAEMON_IFACE = 'dev.ronak.Daemon1';

// The extension owns this name; the daemon calls into it.
export const SHELL_NAME = 'dev.ronak.Shell1';
export const SHELL_PATH = '/dev/ronak/Shell1';

export const SHELL_IFACE_XML = `
<node>
  <interface name="dev.ronak.Shell1">
    <method name="Execute">
      <arg type="s" direction="in" name="command"/>
      <arg type="s" direction="out" name="result"/>
    </method>
    <method name="GetSnapshot">
      <arg type="s" direction="out" name="snapshot"/>
    </method>
    <property name="ProtocolVersion" type="u" access="read"/>
  </interface>
</node>`;

/**
 * Coalesces queued events: a newer `window_changed` for the same window
 * replaces the older one in place (window drags produce dozens per second).
 *
 * @param {object[]} queue
 * @param {object} event
 */
export function enqueue(queue, event) {
    if (event.type === 'window_changed') {
        const i = queue.findIndex(e => e.type === 'window_changed' && e.window.id === event.window.id);
        if (i >= 0) {
            queue[i] = event;
            return;
        }
    }
    queue.push(event);
}

/** Expands a leading "~/" to the home directory. */
export function expandHome(path, home) {
    return path.startsWith('~/') ? `${home}${path.slice(1)}` : path;
}
