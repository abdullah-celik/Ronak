// SPDX-License-Identifier: GPL-2.0-or-later
//
// Pure helpers for schema 0.2: notifications and the Ronak panel window.
// Free of GNOME imports so they can be tested with Node.

import {evaluate} from './privacy.js';

/** The panel app's id (Wayland app id, GApplication id). */
export const PANEL_APP_ID = 'dev.ronak.Panel';

/**
 * True for windows of the Ronak panel. They are never reported to the
 * daemon: Ronak does not observe or arrange itself.
 *
 * @param {(string|null|undefined)[]} ids  gtk application id, wm class, sandboxed app id
 */
export function isPanelWindow(ids) {
    return ids.some(id => typeof id === 'string' && id.toLowerCase() === PANEL_APP_ID.toLowerCase());
}

// MessageTray.Urgency in GNOME: LOW 0, NORMAL 1, HIGH 2, CRITICAL 3.
const URGENCY = ['low', 'normal', 'high', 'critical'];

function clip(text, max) {
    if (typeof text !== 'string')
        return undefined;
    const t = text.replace(/\s+/g, ' ').trim();
    if (!t)
        return undefined;
    return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

/**
 * Turns a notification into a protocol event, applying the exclusion rules
 * to the sending app first: an ignored app's notifications never leave
 * gnome-shell, a redacted one's leave without title and body.
 *
 * @param {object[]} rules  exclusion rules from the daemon
 * @param {{appId?: string, appName: string, title?: string, body?: string, urgency?: number, at: string}} n
 * @returns {object|null}
 */
export function notificationEvent(rules, n) {
    const appId = n.appId || n.appName || 'unknown';
    // Rules match on the app id and, for title rules, on the notification title.
    const mode = evaluate(rules, {app: {id: appId}, title: n.title});
    if (mode === 'ignore')
        return null;
    const notification = {
        app: clip(n.appName, 80) || appId,
        at: n.at,
        urgency: URGENCY[n.urgency] ?? 'normal',
    };
    if (mode !== 'redact') {
        const title = clip(n.title, 200);
        const body = clip(n.body, 600);
        if (title)
            notification.title = title;
        if (body)
            notification.body = body;
    }
    return {type: 'notification', notification};
}

/**
 * Where the panel goes. Side panel: docked to the right edge of the work
 * area, full height (layouts leave that strip free). Floating card: near the
 * top-right corner, keeping its own height within the work area.
 *
 * @param {'side_panel'|'floating_card'} style
 * @param {number} width
 * @param {{x: number, y: number, w: number, h: number}} area  work area
 * @param {number} currentHeight
 */
export function panelGeometry(style, width, area, currentHeight) {
    const w = Math.max(240, Math.min(width, area.w));
    if (style === 'floating_card') {
        const margin = 12;
        const h = Math.max(200, Math.min(currentHeight || 480, area.h - 2 * margin));
        return {x: area.x + area.w - w - margin, y: area.y + margin, w, h};
    }
    return {x: area.x + area.w - w, y: area.y, w, h: area.h};
}
