// SPDX-License-Identifier: GPL-2.0-or-later
// Run with: cd extension && npm test   (or: node --test test/)
import {test} from 'node:test';
import assert from 'node:assert/strict';

import {isPanelWindow, notificationEvent, panelGeometry} from '../ronak@ronak.dev/lib/surfaces.js';

const rules = [
    {app_id: '*keepassxc*', mode: 'ignore'},
    {app_id: 'org.gnome.Fractal', mode: 'redact'},
    {title_contains: 'verification code', mode: 'ignore'},
];
const at = '2026-11-09T10:00:00Z';

test('notifications carry app, title and body', () => {
    const e = notificationEvent(rules, {appId: 'com.slack.Slack', appName: 'Slack', title: 'Rojin',
        body: 'can you   review\nmy PR?', urgency: 2, at});
    assert.deepEqual(e, {type: 'notification', notification: {
        app: 'Slack', at, urgency: 'high', title: 'Rojin', body: 'can you review my PR?'}});
});

test('exclusion rules apply to notifications before they leave the shell', () => {
    assert.equal(notificationEvent(rules, {appId: 'org.keepassxc.KeePassXC', appName: 'KeePassXC', title: 'x', at}), null);
    assert.equal(notificationEvent(rules, {appName: 'Bank', title: 'Your verification code', body: '123456', at}), null);
    const r = notificationEvent(rules, {appId: 'org.gnome.Fractal', appName: 'Fractal', title: 'Ali', body: 'hi', at});
    assert.deepEqual(r.notification, {app: 'Fractal', at, urgency: 'normal'});
});

test('long text is clipped and empty fields are dropped', () => {
    const e = notificationEvent([], {appName: 'App', title: '  ', body: 'x'.repeat(1000), urgency: 9, at});
    assert.equal(e.notification.title, undefined);
    assert.equal(e.notification.body.length, 600);
    assert.equal(e.notification.urgency, 'normal');
});

test('the panel window is recognised by any of its ids', () => {
    assert.ok(isPanelWindow([null, 'dev.ronak.Panel', undefined]));
    assert.ok(isPanelWindow(['DEV.RONAK.PANEL']));
    assert.ok(!isPanelWindow(['org.gnome.Nautilus', null]));
});

test('panel geometry: docked side panel and floating card', () => {
    const area = {x: 0, y: 32, w: 1920, h: 1168};
    assert.deepEqual(panelGeometry('side_panel', 360, area, 700), {x: 1560, y: 32, w: 360, h: 1168});
    assert.deepEqual(panelGeometry('floating_card', 360, area, 500), {x: 1548, y: 44, w: 360, h: 500});
    // A tall floating card is kept inside the work area.
    assert.equal(panelGeometry('floating_card', 360, area, 5000).h, 1144);
    // Never wider than the screen.
    assert.equal(panelGeometry('side_panel', 4000, {x: 0, y: 0, w: 800, h: 600}, 0).w, 800);
});
