// SPDX-License-Identifier: GPL-2.0-or-later
//
// Exclusion rules, applied inside gnome-shell so that excluded data never
// crosses the D-Bus boundary. Mirrors crates/ronak-core/src/privacy.rs
// exactly; both are tested against tests/fixtures/exclusions.json.
//
// This file must stay free of GNOME imports so it can be tested with Node.

/**
 * Case-insensitive glob where `*` matches any (possibly empty) run of characters.
 *
 * @param {string} pattern
 * @param {string} text
 * @returns {boolean}
 */
export function globMatch(pattern, text) {
    const p = [...pattern.toLowerCase()];
    const t = [...text.toLowerCase()];
    let pi = 0, ti = 0, star = -1, mark = 0;
    while (ti < t.length) {
        if (pi < p.length && p[pi] !== '*' && p[pi] === t[ti]) {
            pi++;
            ti++;
        } else if (pi < p.length && p[pi] === '*') {
            star = pi++;
            mark = ti;
        } else if (star >= 0) {
            pi = star + 1;
            ti = ++mark;
        } else {
            return false;
        }
    }
    while (pi < p.length && p[pi] === '*')
        pi++;
    return pi === p.length;
}

/**
 * A rule matches when every condition it sets matches. No conditions: no match.
 *
 * @param {{app_id?: string, title_contains?: string, mode: string}} rule
 * @param {{app?: {id: string}, title?: string}} win
 * @returns {boolean}
 */
export function matchRule(rule, win) {
    if (rule.app_id == null && rule.title_contains == null)
        return false;
    if (rule.app_id != null) {
        if (!win.app || !globMatch(rule.app_id, win.app.id))
            return false;
    }
    if (rule.title_contains != null) {
        if (typeof win.title !== 'string' ||
            !win.title.toLowerCase().includes(rule.title_contains.toLowerCase()))
            return false;
    }
    return true;
}

const STRENGTH = {redact: 1, ignore: 2};

/**
 * The strongest mode among matching rules: 'ignore' beats 'redact'.
 *
 * @returns {null | 'redact' | 'ignore'}
 */
export function evaluate(rules, win) {
    let best = null;
    for (const rule of rules ?? []) {
        if (matchRule(rule, win) && (best === null || STRENGTH[rule.mode] > STRENGTH[best]))
            best = rule.mode;
    }
    return best;
}

/**
 * Returns a copy of `win` with sensitive fields removed, or null if the
 * window must not be reported at all.
 */
export function applyExclusions(rules, win, observeTitles = true) {
    const mode = evaluate(rules, win);
    if (mode === 'ignore')
        return null;
    const out = {...win};
    if (mode === 'redact') {
        delete out.title;
        out.redacted = true;
    } else if (!observeTitles) {
        delete out.title;
    }
    return out;
}
