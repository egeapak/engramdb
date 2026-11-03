// Pieces shared by several views.

import { h } from '../dom.js';
import { warnGlyph } from '../lines.js';

export function errorBox(e) {
  const status = e && e.status;
  let title = 'Something went wrong';
  if (status === 404) title = 'Not found';
  else if (status === 409) title = 'Unavailable';
  else if (status === 401) title = 'Not signed in';
  else if (status === 0) title = 'Disconnected';
  return h('div', { class: 'error-box', role: 'alert' }, h('b', null, title), h('p', null, (e && e.message) || String(e)));
}

export function secHead(title, ...aside) {
  return h('div', { class: 'sec-head' }, h('h2', null, title), aside.length ? h('div', { class: 'sec-aside' }, aside) : null);
}

export function projHead(eyebrow, title, metaLines) {
  return h(
    'div',
    { class: 'proj-head' },
    h('div', null, h('div', { class: 'eyebrow' }, eyebrow), h('h1', { id: 'view-title' }, title)),
    h('div', { class: 'meta' }, (metaLines || []).flatMap((l, i) => (i ? [h('br'), l] : [l]))),
  );
}

/** Status / epistemic pill. */
export function pill(text, cls) {
  return h('span', { class: ['ep', cls] }, text);
}

export function statusPill(row) {
  if (row.invalidated_at && Date.parse(row.invalidated_at) <= Date.now()) return pill('invalidated', 'invalidated');
  if (row.status === 'challenged') return pill('challenged', 'challenged');
  if (row.status === 'needs_review') return pill('needs review', 'needs_review');
  return null;
}

/** The "service disruption" banner for challenged memories. */
export function disruption(text, why, action) {
  return h(
    'div',
    { class: 'disrupt', role: 'status' },
    warnGlyph(22),
    h('span', null, h('b', null, 'Service disruption: '), text),
    action || null,
    why ? h('span', { class: 'why' }, why) : null,
  );
}

export function schemaNote(schema, staleness) {
  const notes = [];
  if (schema && schema.needs_migration) {
    notes.push(
      h(
        'div',
        { class: 'note warn' },
        `This store's schema (${schema.stored || 'unknown'}) is behind this binary's (${schema.current}). `,
        'The dashboard only reads, so it does not migrate; run any engramdb command in the project to upgrade it. ',
        'Some figures may be missing until then.',
      ),
    );
  }
  if (staleness) notes.push(h('div', { class: 'note warn' }, staleness));
  return notes;
}
