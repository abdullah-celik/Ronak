// SPDX-License-Identifier: GPL-2.0-or-later
//
// Ronak GNOME Shell extension: the "eyes and hands" of ronakd.
//
// On Wayland only the compositor can see and move every window. GNOME's
// compositor (Mutter) runs inside the gnome-shell process, and extensions
// are JavaScript loaded into that same process. So this file can do what no
// ordinary app can: list all windows, watch focus, and move them.
//
// It does as little as possible:
//   eyes:  observe windows, focus, workspaces, monitors and idle time,
//          apply privacy rules, and send the result to ronakd.
//   hands: run the actions ronakd asks for (place, focus, do-not-disturb),
//          and position the Ronak panel window (Wayland apps cannot place
//          their own windows, so the panel app relies on us for that).
//   ui:    a small top-bar indicator and four keybindings.
// Schema 0.2 adds: each window's process id (so the daemon can read inside
// the app through the accessibility bus), and notifications (app, title,
// body), after the same exclusion rules as windows.
// All thinking happens in the daemon, so a bug there can never freeze your
// desktop.
//
// Trust: we only talk to a process owning dev.ronak.Daemon1 whose executable
// is in the `trusted-daemon-paths` setting, and we only accept commands from
// that same connection. Events are direct method calls to it, never
// broadcast signals.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as Config from 'resource:///org/gnome/shell/misc/config.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {applyExclusions} from './lib/privacy.js';
import * as P from './lib/protocol.js';
import {isPanelWindow, notificationEvent, panelGeometry} from './lib/surfaces.js';

const FLUSH_MS = 250;
const KEYBINDINGS = ['accept-suggestion', 'undo-layout', 'toggle-observing', 'toggle-panel'];

// ----- small helpers -------------------------------------------------------

function rectOf(r) {
    return {x: r.x, y: r.y, w: r.width, h: r.height};
}

function kindOf(win) {
    switch (win.get_window_type()) {
    case Meta.WindowType.NORMAL:
        return 'normal';
    case Meta.WindowType.DIALOG:
    case Meta.WindowType.MODAL_DIALOG:
        return 'dialog';
    case Meta.WindowType.UTILITY:
        return 'utility';
    default:
        return 'other';
    }
}

const TRACKED_TYPES = [
    Meta.WindowType.NORMAL,
    Meta.WindowType.DIALOG,
    Meta.WindowType.MODAL_DIALOG,
    Meta.WindowType.UTILITY,
];

// Mutter's maximize API changed over time; these keep GNOME 46-48 working.
function isMaximized(win) {
    if (typeof win.get_maximized === 'function')
        return win.get_maximized() === Meta.MaximizeFlags.BOTH;
    return Boolean(win.is_maximized?.());
}

function unmaximize(win) {
    if (!win.maximized_horizontally && !win.maximized_vertically && !isMaximized(win))
        return;
    if (Meta.MaximizeFlags)
        win.unmaximize(Meta.MaximizeFlags.BOTH);
    else
        win.unmaximize();
}

function maximize(win) {
    if (Meta.MaximizeFlags)
        win.maximize(Meta.MaximizeFlags.BOTH);
    else
        win.maximize();
}

function dbusCall(conn, dest, path, iface, method, params, replyType) {
    return new Promise((resolve, reject) => {
        conn.call(dest, path, iface, method, params,
            replyType ? new GLib.VariantType(replyType) : null,
            Gio.DBusCallFlags.NONE, 5000, null,
            (c, res) => {
                try {
                    resolve(c.call_finish(res));
                } catch (e) {
                    reject(e);
                }
            });
    });
}

// ----- top-bar indicator ---------------------------------------------------

const RonakIndicator = GObject.registerClass(
class RonakIndicator extends PanelMenu.Button {
    _init(ext) {
        super._init(0.0, 'Ronak');
        this._ext = ext;

        const box = new St.BoxLayout({style_class: 'panel-status-menu-box'});
        this._icon = new St.Icon({icon_name: 'view-grid-symbolic', style_class: 'system-status-icon'});
        this._label = new St.Label({text: '', y_align: Clutter.ActorAlign.CENTER, style_class: 'ronak-label'});
        box.add_child(this._icon);
        box.add_child(this._label);
        this.add_child(box);

        this._status = new PopupMenu.PopupMenuItem('', {reactive: false});
        this.menu.addMenuItem(this._status);

        this._applyItem = new PopupMenu.PopupMenuItem('');
        this._applyItem.connect('activate', () => ext.acceptSuggestion());
        this.menu.addMenuItem(this._applyItem);

        this._dismissItem = new PopupMenu.PopupMenuItem('Dismiss suggestion');
        this._dismissItem.connect('activate', () => ext.dismissSuggestion());
        this.menu.addMenuItem(this._dismissItem);

        const undo = new PopupMenu.PopupMenuItem('Undo last change (Super+Alt+Z)');
        undo.connect('activate', () => ext.undo());
        this.menu.addMenuItem(undo);

        const panel = new PopupMenu.PopupMenuItem('Show or hide the Ronak panel (Super+Alt+Space)');
        panel.connect('activate', () => ext.togglePanel());
        this.menu.addMenuItem(panel);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._observeItem = new PopupMenu.PopupSwitchMenuItem('Observe this desktop', ext.observing);
        this._observeItem.connect('toggled', (_item, state) => ext.setObserving(state));
        this.menu.addMenuItem(this._observeItem);

        this.setConnection('waiting');
        this.clearSuggestion();
    }

    setConnection(state) {
        const text = {
            waiting: 'Ronak: waiting for ronakd',
            connected: 'Ronak: connected',
            untrusted: 'Ronak: refused an untrusted ronakd (see logs)',
        }[state] ?? `Ronak: ${state}`;
        this._status.label.text = text;
    }

    setObserving(on) {
        this._observeItem.setToggleState(on);
        if (on)
            this.remove_style_class_name('ronak-paused');
        else
            this.add_style_class_name('ronak-paused');
    }

    showSuggestion(s) {
        this._label.text = s.task_label;
        this._applyItem.label.text = `Apply: ${s.summary} (Super+Alt+Enter)`;
        this._applyItem.setSensitive(true);
        this._dismissItem.setSensitive(true);
        this.add_style_class_name('ronak-has-suggestion');
    }

    clearSuggestion() {
        this._label.text = '';
        this._applyItem.label.text = 'No layout suggestion right now';
        this._applyItem.setSensitive(false);
        this._dismissItem.setSensitive(false);
        this.remove_style_class_name('ronak-has-suggestion');
    }
});

// ----- the D-Bus object ronakd calls --------------------------------------

class ShellService {
    constructor(ext) {
        this._ext = ext;
    }

    ExecuteAsync([json], invocation) {
        if (!this._ext.isFromDaemon(invocation)) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied', 'not the trusted ronakd');
            return;
        }
        let result;
        try {
            result = this._ext.execute(JSON.parse(json));
        } catch (e) {
            result = {ok: false, errors: [String(e)]};
        }
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify(result)]));
    }

    GetSnapshotAsync(_params, invocation) {
        if (!this._ext.isFromDaemon(invocation)) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied', 'not the trusted ronakd');
            return;
        }
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify(this._ext.snapshot())]));
    }

    get ProtocolVersion() {
        return P.PROTOCOL_VERSION;
    }
}

// ----- the extension -------------------------------------------------------

export default class RonakExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._notifSettings = new Gio.Settings({schema_id: 'org.gnome.desktop.notifications'});
        this._tracker = Shell.WindowTracker.get_default();
        this._signals = [];           // [object, handlerId]
        this._windowSignals = new Map(); // MetaWindow -> [handlerId]
        this._sources = new Set();    // GLib source ids
        this._known = new Set();      // window ids the daemon knows about
        this._pending = [];
        this._rules = [];
        this._observeTitles = true;
        this._activityMs = 15000;
        this._daemonOwner = null;
        this._daemonReady = false;
        this._suggestion = null;
        this._observeNotifications = true;
        this._traySignals = new Map(); // MessageTray.Source -> [handlerId]
        this._panelWin = null;
        this._panelSignals = [];
        this._panelState = {visible: false, style: 'side_panel', width: 360};

        this._indicator = new RonakIndicator(this);
        this._indicator.setObserving(this.observing);
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        this._exportService();
        this._connectDesktopSignals();
        for (const win of this._allWindows()) {
            this._track(win);
            this._maybeAdoptPanel(win);
        }
        this._watchNotifications();
        this._addKeybindings();

        this._watchId = Gio.bus_watch_name(Gio.BusType.SESSION, P.DAEMON_NAME,
            Gio.BusNameWatcherFlags.NONE,
            (_conn, _name, owner) => {
                this._onDaemonAppeared(owner).catch(e => this._warn(`connecting to ronakd failed: ${e}`));
            },
            () => this._onDaemonVanished());
    }

    disable() {
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        for (const name of KEYBINDINGS)
            Main.wm.removeKeybinding(name);
        for (const id of this._sources)
            GLib.source_remove(id);
        this._sources.clear();
        this._flushId = 0;
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        for (const [win, ids] of this._windowSignals)
            ids.forEach(id => win.disconnect(id));
        this._windowSignals.clear();
        for (const [source, ids] of this._traySignals)
            ids.forEach(id => source.disconnect(id));
        this._traySignals.clear();
        this._releasePanel();
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        this._dbusImpl?.unexport();
        this._dbusImpl = null;
        this._indicator?.destroy();
        this._indicator = null;
        this._settings = null;
        this._notifSettings = null;
        this._tracker = null;
        this._known.clear();
        this._pending = [];
        this._daemonOwner = null;
        this._daemonReady = false;
    }

    get observing() {
        return this._settings.get_boolean('observing');
    }

    // ----- logging and timers --------------------------------------------

    _log(msg) {
        console.log(`[ronak] ${msg}`);
    }

    _warn(msg) {
        console.warn(`[ronak] ${msg}`);
    }

    _later(ms, fn) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._sources.delete(id);
            fn();
            return GLib.SOURCE_REMOVE;
        });
        this._sources.add(id);
        return id;
    }

    _connect(obj, signal, fn) {
        this._signals.push([obj, obj.connect(signal, fn)]);
    }

    // ----- eyes: observing the desktop -----------------------------------

    _allWindows() {
        if (typeof global.display.list_all_windows === 'function')
            return global.display.list_all_windows();
        return global.get_window_actors().map(a => a.meta_window);
    }

    _shouldTrack(win) {
        if (win.is_override_redirect?.() || win.is_skip_taskbar?.())
            return false;
        if (this._isPanel(win))
            return false;
        return TRACKED_TYPES.includes(win.get_window_type());
    }

    _isPanel(win) {
        return isPanelWindow([win.get_gtk_application_id?.(), win.get_wm_class?.(), win.get_sandboxed_app_id?.()]);
    }

    _describe(win) {
        const app = this._tracker.get_window_app(win);
        let appId = app?.get_id?.();
        appId = appId
            ? appId.replace(/\.desktop$/, '')
            : (win.get_sandboxed_app_id?.() || win.get_gtk_application_id?.() || win.get_wm_class() || 'unknown');
        const name = app?.get_name?.() || win.get_wm_class() || appId;
        const workspace = win.is_on_all_workspaces() ? null : (win.get_workspace()?.index() ?? null);
        const desc = {
            id: String(win.get_id()),
            app: {id: appId, name},
            kind: kindOf(win),
            rect: rectOf(win.get_frame_rect()),
            monitor: Math.max(0, win.get_monitor()),
            workspace,
            focused: win.has_focus(),
            minimized: win.minimized,
            maximized: isMaximized(win),
            fullscreen: win.is_fullscreen(),
        };
        // The process id lets the daemon read inside the app (AT-SPI).
        const pid = win.get_pid?.() ?? -1;
        if (pid > 0)
            desc.pid = pid;
        const title = win.get_title();
        if (title)
            desc.title = title;
        return desc;
    }

    /** The window as it may leave gnome-shell, or null if excluded. */
    _export(win) {
        return applyExclusions(this._rules, this._describe(win), this._observeTitles);
    }

    _track(win) {
        if (this._windowSignals.has(win) || !this._shouldTrack(win))
            return;
        const changed = () => this._reportWindow(win);
        const ids = [
            win.connect('notify::title', changed),
            win.connect('position-changed', changed),
            win.connect('size-changed', changed),
            win.connect('notify::minimized', changed),
            win.connect('workspace-changed', changed),
            win.connect('unmanaged', () => this._untrack(win)),
        ];
        this._windowSignals.set(win, ids);
        // Freshly created windows often have no title or app yet; report
        // once the main loop has let them finish setting up.
        this._later(50, () => this._reportWindow(win));
    }

    _untrack(win) {
        const ids = this._windowSignals.get(win);
        if (ids)
            ids.forEach(id => win.disconnect(id));
        this._windowSignals.delete(win);
        const id = String(win.get_id());
        if (this._known.delete(id))
            this._queue({type: 'window_closed', window_id: id});
    }

    _reportWindow(win) {
        if (!this._windowSignals.has(win))
            return;
        if (this._isPanel(win)) {
            // Its app id arrived late; it is ours, so stop tracking it.
            this._untrack(win);
            this._maybeAdoptPanel(win);
            return;
        }
        const id = String(win.get_id());
        const desc = this._export(win);
        if (desc === null) {
            // Excluded (perhaps it just switched to private browsing).
            if (this._known.delete(id))
                this._queue({type: 'window_closed', window_id: id});
            return;
        }
        const type = this._known.has(id) ? 'window_changed' : 'window_opened';
        this._known.add(id);
        this._queue({type, window: desc});
        // A new window usually gets focus before we report it (we wait for
        // its app id), and that focus change was sent as "nothing focused".
        if (type === 'window_opened' && win.has_focus())
            this._queue({type: 'focus_changed', window_id: id});
    }

    _focusedId() {
        const win = global.display.focus_window;
        const id = win ? String(win.get_id()) : null;
        return id && this._known.has(id) ? id : null;
    }

    _monitors() {
        const n = global.display.get_n_monitors();
        const primary = global.display.get_primary_monitor();
        const ws = global.workspace_manager.get_active_workspace();
        const out = [];
        for (let i = 0; i < n; i++) {
            out.push({
                index: i,
                geometry: rectOf(global.display.get_monitor_geometry(i)),
                work_area: rectOf(ws.get_work_area_for_monitor(i)),
                scale: global.display.get_monitor_scale(i),
                primary: i === primary,
            });
        }
        return out;
    }

    _workspaces() {
        const wm = global.workspace_manager;
        return {active: wm.get_active_workspace_index(), count: wm.get_n_workspaces()};
    }

    _idleMs() {
        try {
            return global.backend.get_core_idle_monitor().get_idletime();
        } catch {
            return 0;
        }
    }

    _session() {
        return {
            do_not_disturb: !this._notifSettings.get_boolean('show-banners'),
            observing: this.observing,
        };
    }

    snapshot() {
        const windows = [];
        for (const win of this._allWindows()) {
            if (!this._windowSignals.has(win))
                continue;
            const desc = this._export(win);
            if (desc)
                windows.push(desc);
        }
        this._known = new Set(windows.map(w => w.id));
        return {
            type: 'snapshot',
            monitors: this._monitors(),
            workspaces: this._workspaces(),
            windows,
            focused_window: this._focusedId(),
            idle_ms: this._idleMs(),
            session: this._session(),
        };
    }

    _connectDesktopSignals() {
        this._connect(global.display, 'window-created', (_d, win) => {
            this._track(win);
            // Wayland apps may set their app id just after the window appears.
            this._later(50, () => this._maybeAdoptPanel(win));
        });
        this._connect(global.display, 'notify::focus-window', () =>
            this._queue({type: 'focus_changed', window_id: this._focusedId()}));
        this._connect(global.display, 'workareas-changed', () =>
            this._queue({type: 'monitors_changed', monitors: this._monitors()}));
        this._connect(Main.layoutManager, 'monitors-changed', () =>
            this._queue({type: 'monitors_changed', monitors: this._monitors()}));
        this._connect(global.workspace_manager, 'active-workspace-changed', () =>
            this._queue({type: 'workspaces_changed', workspaces: this._workspaces()}));
        this._connect(global.workspace_manager, 'notify::n-workspaces', () =>
            this._queue({type: 'workspaces_changed', workspaces: this._workspaces()}));
        this._connect(this._notifSettings, 'changed::show-banners', () =>
            this._queue({type: 'session_changed', session: this._session()}));
        this._connect(this._settings, 'changed::observing', () => this._onObservingChanged());
    }

    // ----- talking to ronakd ---------------------------------------------

    _queue(event) {
        if (!this._daemonReady || !this.observing)
            return;
        P.enqueue(this._pending, event);
        if (!this._flushId) {
            this._flushId = this._later(FLUSH_MS, () => {
                this._flushId = 0;
                this._flush();
            });
        }
    }

    _flush() {
        if (!this._pending.length || !this._daemonReady)
            return;
        const batch = this._pending;
        this._pending = [];
        this._push(batch);
    }

    _push(events) {
        this._callDaemon('PushEvents', JSON.stringify(events), null)
            .catch(e => this._warn(`sending events failed: ${e}`));
    }

    _callDaemon(method, arg, replyType) {
        return dbusCall(Gio.DBus.session, this._daemonOwner, P.DAEMON_PATH, P.DAEMON_IFACE,
            method, new GLib.Variant('(s)', [arg]), replyType);
    }

    async _isTrustedDaemon(owner) {
        try {
            const reply = await dbusCall(Gio.DBus.session, 'org.freedesktop.DBus', '/org/freedesktop/DBus',
                'org.freedesktop.DBus', 'GetConnectionUnixProcessID', new GLib.Variant('(s)', [owner]), '(u)');
            const [pid] = reply.deepUnpack();
            const exe = GLib.file_read_link(`/proc/${pid}/exe`).replace(/ \(deleted\)$/, '');
            const home = GLib.get_home_dir();
            const trusted = this._settings.get_strv('trusted-daemon-paths').map(p => P.expandHome(p, home));
            if (trusted.includes(exe))
                return true;
            this._warn(`refusing ${P.DAEMON_NAME} owned by ${exe}; trusted: ${trusted.join(', ')}`);
            return false;
        } catch (e) {
            this._warn(`could not verify ronakd: ${e}`);
            return false;
        }
    }

    async _onDaemonAppeared(owner) {
        if (!(await this._isTrustedDaemon(owner))) {
            this._indicator?.setConnection('untrusted');
            return;
        }
        if (!this._settings)
            return; // disabled while we were checking
        this._daemonOwner = owner;
        const hello = {
            type: 'hello',
            protocol_version: P.PROTOCOL_VERSION,
            platform: {
                os: 'linux',
                desktop: 'gnome',
                desktop_version: Config.PACKAGE_VERSION,
                adapter: 'ronak-gnome-extension',
                adapter_version: this.metadata['version-name'] ?? String(this.metadata.version ?? '0'),
            },
        };
        const reply = await this._callDaemon('Hello', JSON.stringify(hello), '(s)');
        const [configJson] = reply.deepUnpack();
        this.applyConfig(JSON.parse(configJson));
        this._daemonReady = true;
        this._indicator?.setConnection('connected');
        this._log('connected to ronakd');
        if (this.observing)
            this._push([this.snapshot()]);
        this._scheduleActivity();
    }

    _onDaemonVanished() {
        if (this._daemonOwner)
            this._log('ronakd went away');
        this._daemonOwner = null;
        this._daemonReady = false;
        this._pending = [];
        this._suggestion = null;
        this._indicator?.clearSuggestion();
        this._indicator?.setConnection('waiting');
    }

    _scheduleActivity() {
        this._later(this._activityMs, () => {
            if (!this._daemonReady)
                return;
            this._queue({type: 'activity', idle_ms: this._idleMs()});
            this._scheduleActivity();
        });
    }

    applyConfig(config) {
        this._rules = config.exclusions ?? [];
        this._observeTitles = config.observe_titles ?? true;
        this._activityMs = Math.max(1000, config.activity_interval_ms ?? 15000);
        this._observeNotifications = config.observe_notifications ?? true;
    }

    // ----- notifications ---------------------------------------------------

    _watchNotifications() {
        const tray = Main.messageTray;
        if (!tray)
            return;
        this._connect(tray, 'source-added', (_tray, source) => this._watchSource(source));
        for (const source of tray.getSources?.() ?? [])
            this._watchSource(source);
    }

    _watchSource(source) {
        if (this._traySignals.has(source))
            return;
        const ids = [
            source.connect('notification-added', (src, notification) => this._onNotification(src, notification)),
            source.connect('destroy', () => this._traySignals.delete(source)),
        ];
        this._traySignals.set(source, ids);
    }

    _onNotification(source, notification) {
        if (!this._observeNotifications || !this._daemonReady || !this.observing)
            return;
        try {
            const app = source.app ?? source._app ?? null;
            let appId = app?.get_id?.()?.replace(/\.desktop$/, '');
            const policyId = source.policy?.id;
            if (!appId && policyId && policyId !== 'generic')
                appId = policyId;
            const at = (notification.datetime ?? GLib.DateTime.new_now_local()).to_utc().format_iso8601();
            const event = notificationEvent(this._rules, {
                appId,
                appName: source.title || app?.get_name?.() || 'unknown',
                title: notification.title,
                body: notification.body,
                urgency: notification.urgency,
                at,
            });
            if (event)
                this._queue(event);
        } catch (e) {
            this._warn(`could not read a notification: ${e}`);
        }
    }

    // ----- the Ronak panel window ----------------------------------------

    _maybeAdoptPanel(win) {
        if (!this._settings || this._panelWin === win || !this._isPanel(win))
            return;
        this._releasePanel();
        this._panelWin = win;
        this._panelSignals = [
            win.connect('unmanaged', () => this._releasePanel()),
            win.connect('shown', () => this._arrangePanel()),
        ];
        this._arrangePanel();
    }

    _releasePanel() {
        if (this._panelWin)
            this._panelSignals.forEach(id => this._panelWin.disconnect(id));
        this._panelWin = null;
        this._panelSignals = [];
    }

    _arrangePanel() {
        const win = this._panelWin;
        const {visible, style, width} = this._panelState;
        if (!win || !visible)
            return; // the panel app hides its own window
        const monitor = Main.layoutManager.primaryIndex;
        const area = rectOf(global.workspace_manager.get_active_workspace().get_work_area_for_monitor(monitor));
        const place = () => {
            if (this._panelWin !== win)
                return;
            const g = panelGeometry(style, width, area, win.get_frame_rect().height);
            unmaximize(win);
            win.make_above();
            if (!win.is_on_all_workspaces())
                win.stick();
            win.move_resize_frame(false, g.x, g.y, g.w, g.h);
        };
        place();
        // As with layouts, Wayland clients may answer asynchronously.
        this._later(60, place);
        this._later(400, place);
    }

    isFromDaemon(invocation) {
        return this._daemonReady && invocation.get_sender() === this._daemonOwner;
    }

    // ----- hands: carrying out commands ----------------------------------

    _exportService() {
        this._dbusImpl = Gio.DBusExportedObject.wrapJSObject(P.SHELL_IFACE_XML, new ShellService(this));
        this._dbusImpl.export(Gio.DBus.session, P.SHELL_PATH);
        this._ownerId = Gio.bus_own_name_on_connection(Gio.DBus.session, P.SHELL_NAME,
            Gio.BusNameOwnerFlags.NONE, null, null);
    }

    execute(cmd) {
        switch (cmd.cmd) {
        case 'execute':
            return this._executeActions(cmd.actions ?? []);
        case 'show_suggestion':
            this._suggestion = cmd.suggestion;
            this._indicator?.showSuggestion(cmd.suggestion);
            return {ok: true};
        case 'clear_suggestion':
            this._suggestion = null;
            this._indicator?.clearSuggestion();
            return {ok: true};
        case 'configure':
            this.applyConfig(cmd.config);
            return {ok: true};
        case 'arrange_panel':
            this._panelState = {visible: cmd.visible, style: cmd.style, width: cmd.width};
            this._arrangePanel();
            return {ok: true};
        default:
            return {ok: false, errors: [`unknown command '${cmd.cmd}'`]};
        }
    }

    _executeActions(actions) {
        const errors = [];
        for (const action of actions) {
            try {
                this._run(action);
            } catch (e) {
                errors.push(`${action.op}: ${e.message ?? e}`);
            }
        }
        return {ok: errors.length === 0, errors};
    }

    _window(id) {
        const win = this._allWindows().find(w => String(w.get_id()) === id);
        if (!win)
            throw new Error(`no window ${id}`);
        return win;
    }

    _run(action) {
        const now = global.get_current_time();
        switch (action.op) {
        case 'place':
            for (const p of action.placements)
                this._place(p);
            break;
        case 'focus':
            Main.activateWindow(this._window(action.window_id));
            break;
        case 'minimize':
            this._window(action.window_id).minimize();
            break;
        case 'unminimize':
            this._window(action.window_id).unminimize();
            break;
        case 'move_to_workspace':
            this._window(action.window_id).change_workspace_by_index(action.workspace, false);
            break;
        case 'activate_workspace':
            global.workspace_manager.get_workspace_by_index(action.workspace)?.activate(now);
            break;
        case 'create_workspace':
            global.workspace_manager.append_new_workspace(false, now);
            break;
        case 'set_do_not_disturb':
            this._notifSettings.set_boolean('show-banners', !action.enabled);
            break;
        default:
            throw new Error(`unknown action '${action.op}'`);
        }
    }

    _place(p) {
        const win = this._window(p.window_id);
        if (p.maximized) {
            maximize(win);
            return;
        }
        if (win.is_fullscreen())
            win.unmake_fullscreen();
        unmaximize(win);
        const {x, y, w, h} = p.rect;
        win.move_resize_frame(false, x, y, w, h);
        // Wayland clients answer an unmaximize asynchronously and may restore
        // their old size afterwards; apply the rectangle once more shortly.
        this._later(60, () => {
            if (this._windowSignals.has(win))
                win.move_resize_frame(false, x, y, w, h);
        });
    }

    // ----- user actions (keybindings and menu) ---------------------------

    _addKeybindings() {
        const add = (name, fn) => Main.wm.addKeybinding(name, this._settings,
            Meta.KeyBindingFlags.NONE, Shell.ActionMode.NORMAL, fn);
        add('accept-suggestion', () => this.acceptSuggestion());
        add('undo-layout', () => this.undo());
        add('toggle-observing', () => this.setObserving(!this.observing));
        add('toggle-panel', () => this.togglePanel());
    }

    _sendUser(action) {
        if (!this._daemonReady) {
            Main.notify('Ronak', 'ronakd is not running.');
            return;
        }
        // User actions bypass the queue (and the pause switch) so they are instant.
        this._push([{type: 'user', action}]);
    }

    acceptSuggestion() {
        if (this._suggestion)
            this._sendUser({kind: 'accept_suggestion', suggestion_id: this._suggestion.id});
    }

    dismissSuggestion() {
        if (this._suggestion)
            this._sendUser({kind: 'dismiss_suggestion', suggestion_id: this._suggestion.id});
    }

    undo() {
        this._sendUser({kind: 'undo'});
    }

    togglePanel() {
        this._sendUser({kind: 'toggle_panel'});
    }

    setObserving(on) {
        this._settings.set_boolean('observing', on);
    }

    _onObservingChanged() {
        const on = this.observing;
        this._indicator?.setObserving(on);
        if (!this._daemonReady)
            return;
        this._push([
            {type: 'user', action: {kind: 'set_observing', observing: on}},
            {type: 'session_changed', session: this._session()},
        ]);
        if (on)
            this._push([this.snapshot()]);
        else
            this._pending = [];
    }
}
