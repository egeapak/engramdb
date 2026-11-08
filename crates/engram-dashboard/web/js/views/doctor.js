// Store health: the read-only half of `engramdb doctor` for one store.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, shortId } from '../format.js';
import { projHead, secHead, schemaNote, errorBox } from './common.js';

function check(state, title, detail, ids) {
  const mark = state === 'ok' ? '✓' : state === 'bad' ? '✕' : '–';
  const said = state === 'ok' ? 'Passed: ' : state === 'bad' ? 'Failed: ' : 'Not checked: ';
  return h(
    'li',
    null,
    h('span', { class: state, 'aria-hidden': 'true' }, mark),
    h('div', null, h('b', null, h('span', { class: 'sr-only' }, said), title), detail ? h('div', { class: 'muted' }, detail) : null),
    h('span', { class: 'mono faint' }, ids ? fmt(ids.length) : ''),
    ids && ids.length
      ? h(
          'div',
          { class: 'ids mono' },
          // Plain text: a stale row's memory has no file, and an orphan has
          // no index row, so neither has a page to link to.
          ids.slice(0, 40).map((id) => shortId(id)).join(', '),
          ids.length > 40 ? ` … and ${fmt(ids.length - 40)} more` : '',
        )
      : null,
  );
}

export async function renderDoctor(main, ctx) {
  const p = ctx.project;
  if (!p.available) {
    mount(main, errorBox({ status: 409, message: `${p.name} is unavailable: ${p.unavailable_reason}` }));
    return;
  }
  mount(main, h('p', { class: 'loading' }, 'Checking every memory file against the index…'));
  const d = await api.doctor(p.id);
  if (ctx.isStale()) return;
  const r = d.result;
  const drift = r.drifted_entries;
  mount(
    main,
    projHead('Health', p.name, [`${fmt(r.indexed)} indexed · ${fmt(r.on_disk)} files on disk`, r.healthy ? 'Healthy' : 'Needs repair: run `engramdb doctor --fix`']),
    schemaNote(d.schema, d.staleness),
    d.checkout_conflict ? h('div', { class: 'note' }, `Another checkout shares this project id (${d.checkout_conflict}); drift is not checked here.`) : null,
    secHead('Index against files'),
    h(
      'ul',
      { class: 'checks' },
      check(r.stale_entries.length ? 'bad' : 'ok', 'Index rows with a file', 'Rows whose .md file is gone are stale.', r.stale_entries),
      check(r.orphaned_files.length ? 'bad' : 'ok', 'Files with an index row', 'Files the index does not know are orphans.', r.orphaned_files),
      drift === null || drift === undefined
        ? check('na', 'Content drift', 'Not checked: no digests to compare, or a shared project id.', null)
        : check(drift.length ? 'bad' : 'ok', 'Content drift', 'Files edited (or restored by git) without the index noticing.', drift),
      check(r.undetermined.length ? 'bad' : 'ok', 'Readable files', 'Files that could not be read or hashed.', r.undetermined),
    ),
    h('p', { class: 'faint' }, 'The dashboard never repairs. `engramdb doctor --fix` in the project does.'),
  );
}
