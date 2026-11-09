// SPDX-License-Identifier: GPL-2.0-or-later
// Run with: cd extension && npm test   (or: node --test test/)
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

import {applyExclusions, evaluate, globMatch} from '../ronak@ronak.dev/lib/privacy.js';
import {enqueue, expandHome, PROTOCOL_VERSION} from '../ronak@ronak.dev/lib/protocol.js';

const fixture = JSON.parse(readFileSync(new URL('../../tests/fixtures/exclusions.json', import.meta.url)));

test('glob matching agrees with the Rust implementation', () => {
    assert.ok(globMatch('*keepassxc*', 'org.keepassxc.KeePassXC'));
    assert.ok(globMatch('firefox', 'Firefox'));
    assert.ok(globMatch('org.*.nautilus', 'org.gnome.Nautilus'));
    assert.ok(!globMatch('firefox', 'firefox-esr'));
    assert.ok(globMatch('*', ''));
    assert.ok(!globMatch('a*b', 'acd'));
});

test('shared exclusion fixture: every case matches Rust', () => {
    for (const c of fixture.cases) {
        const got = evaluate(fixture.rules, c.window) ?? 'keep';
        assert.equal(got, c.expected, c.name);
    }
});

test('redaction removes the title and marks the window', () => {
    const win = {id: '1', app: {id: 'org.keepassxc.KeePassXC', name: 'KeePassXC'}, title: 'bank.kdbx', rect: {x: 0, y: 0, w: 1, h: 1}};
    const out = applyExclusions([{app_id: '*keepassxc*', mode: 'redact'}], win);
    assert.equal(out.title, undefined);
    assert.equal(out.redacted, true);
    assert.equal(win.title, 'bank.kdbx', 'the original object is not modified');
    assert.equal(applyExclusions([{app_id: '*keepassxc*', mode: 'ignore'}], win), null);
    assert.equal(applyExclusions([], win, false).title, undefined, 'titles can be turned off entirely');
});

test('window_changed events coalesce per window', () => {
    const q = [];
    enqueue(q, {type: 'window_changed', window: {id: '1', rect: {x: 0}}});
    enqueue(q, {type: 'focus_changed', window_id: '1'});
    enqueue(q, {type: 'window_changed', window: {id: '1', rect: {x: 5}}});
    enqueue(q, {type: 'window_changed', window: {id: '2', rect: {x: 9}}});
    assert.equal(q.length, 3);
    assert.equal(q[0].window.rect.x, 5, 'the newer state replaces the older one in place');
});

test('protocol constants match the daemon', () => {
    const rust = readFileSync(new URL('../../crates/ronak-core/src/protocol.rs', import.meta.url), 'utf8');
    assert.match(rust, new RegExp(`PROTOCOL_VERSION: u32 = ${PROTOCOL_VERSION};`));
    assert.equal(expandHome('~/.local/bin/ronakd', '/home/me'), '/home/me/.local/bin/ronakd');
    assert.equal(expandHome('/usr/bin/ronakd', '/home/me'), '/usr/bin/ronakd');
});
