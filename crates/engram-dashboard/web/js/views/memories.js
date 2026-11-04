// Memories: a filterable, sortable table with the usage columns.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, daysSince, shortId } from '../format.js';
import { badge, kindIcon, scopeName, TYPE_LABEL } from '../scopes.js';
import { projHead, statusPill, errorBox } from './common.js';

const COLS = [
  { k: 'scope', label: 'Top-level scope' },
  { k: 'summary', label: 'Memory' },
  { k: 'type', label: 'Type' },
  { k: 'returned', label: 'Retrieved', n: true, title: 'Times it survived ranking in a query answer' },
  { k: 'injected', label: 'Injected', n: true, title: 'Times a hook put it into an agent context' },
  { k: 'fetched', label: 'Fetched', n: true, title: 'Times it was opened by id' },
  { k: 'updated_n', label: 'Updates', n: true, title: 'Times it was updated' },
  { k: 'last_read', label: 'Last read', n: true },
  { k: 'criticality', label: 'Criticality', n: true, title: 'How much it matters, 0 to 1' },
];
const TEXT_SORT = new Set(['summary', 'type', 'scope']);
const PAGE = 100;

export async function renderMemories(main, ctx) {
  const p = ctx.project;
  if (!p.available) {
    mount(main, errorBox({ status: 409, message: `${p.name} is unavailable: ${p.unavailable_reason}` }));
    return;
  }
  const q = ctx.route.query;
  const f = {
    q: q.get('q') || '',
    kind: q.get('kind') || '',
    status: q.get('status') || '',
    top_scope: q.get('top_scope') || '',
    sort: q.get('sort') || 'criticality',
    dir: q.get('dir') || '',
  };

  const search = h('input', { class: 'search', type: 'search', placeholder: 'Filter by text, id, tag or any scope', 'aria-label': 'Filter memories', value: f.q });
  const kindSel = h('select', { 'aria-label': 'Type' }, h('option', { value: '' }, 'All types'));
  const statusSel = h(
    'select',
    { 'aria-label': 'Status' },
    [
      ['', 'Live'],
      ['challenged', 'Challenged'],
      ['needs_review', 'Needs review'],
      ['active', 'Active'],
      ['invalidated', 'Invalidated'],
      ['all', 'Everything'],
    ].map(([v, t]) => h('option', { value: v, selected: v === f.status }, t)),
  );
  // Announced politely: the one change a filter makes that a screen-reader
  // user cannot see.
  const count = h('span', { class: 'cnt num', role: 'status', 'aria-live': 'polite' });
  const chips = h('div', { class: 'chips', role: 'group', 'aria-label': 'Filter by top-level scope' });
  const thead = h('thead');
  const tbody = h('tbody', null, h('tr', null, h('td', { colspan: COLS.length, class: 'empty' }, 'Loading memories…')));
  const more = h('div', { class: 'more' });
  const err = h('div');

  mount(
    main,
    projHead('Memories', p.name, ['Counts are lifetime totals.', 'Retrieved, injected and fetched are never added up.']),
    h('div', { class: 'filters' }, search, h('label', { class: 'sel' }, kindSel), h('label', { class: 'sel' }, statusSel), count),
    chips,
    err,
    h('div', { class: 'tbox' }, h('table', null, h('caption', { class: 'sr-only' }, `Memories of ${p.name}`), thead, tbody)),
    more,
  );

  let facetsShown = false; // chips and kinds, redrawn when the status filter changes
  let seq = 0;

  const writeUrl = () => {
    // A debounce timer can fire after the user navigated away: rewriting the
    // hash then would drag them back.
    if (ctx.isStale()) return;
    const u = new URLSearchParams();
    for (const [k, v] of Object.entries(f)) if (v) u.set(k, v);
    if (f.sort === 'criticality') u.delete('sort');
    history.replaceState(null, '', ctx.href('memories', p.id, u.toString()));
  };

  const header = () => {
    mount(
      thead,
      h(
        'tr',
        null,
        COLS.map((c) => {
          const on = f.sort === c.k;
          const desc = f.dir ? f.dir === 'desc' : !TEXT_SORT.has(c.k);
          return h(
            'th',
            { class: c.n ? 'n' : null, 'aria-sort': on ? (desc ? 'descending' : 'ascending') : null, scope: 'col' },
            h(
              'button',
              {
                type: 'button',
                title: c.title || null,
                onclick: () => {
                  if (f.sort === c.k) f.dir = desc ? 'asc' : 'desc';
                  else {
                    f.sort = c.k;
                    f.dir = '';
                  }
                  writeUrl();
                  header();
                  load(false);
                },
              },
              c.label,
              h('span', { class: 'ar', 'aria-hidden': 'true' }, on ? (desc ? '↓' : '↑') : ''),
            ),
          );
        }),
      ),
    );
  };

  const renderChips = (scopes) => {
    // '-' names "no logical scope" in the URL, so an empty name survives a join.
    const key = (name) => name || '-';
    const on = new Set(f.top_scope ? f.top_scope.split(',') : scopes.map((l) => key(l.name)));
    mount(
      chips,
      scopes.map((l) =>
        h(
          'button',
          {
            type: 'button',
            class: 'chip',
            'aria-pressed': String(on.has(key(l.name))),
            onclick: () => {
              if (on.has(key(l.name))) on.delete(key(l.name));
              else on.add(key(l.name));
              f.top_scope = on.size === scopes.length ? '' : Array.from(on).join(',') || '\u2205';
              writeUrl();
              renderChips(scopes);
              load(false);
            },
          },
          badge(l.name, l.slot),
          scopeName(l.name),
          h('span', { class: 'c' }, fmt(l.count)),
        ),
      ),
    );
  };

  const rowEl = (m) => {
    const open = ctx.href('detail', p.id, m.id);
    const last = m.usage.last_read_at;
    const stale = !last || daysSince(last) > 30;
    return h(
      'tr',
      {
        onclick: (ev) => {
          if (ev.target.closest('a')) return;
          location.hash = open;
        },
      },
      h('td', null, badge(m.top_scope, m.color_slot)),
      h('td', { class: 't' }, h('a', { href: open }, m.summary), h('span', { class: 'mid mono' }, `${shortId(m.id)}${m.logical.length ? ' · ' + m.logical.join(', ') : ''}`)),
      h('td', null, h('span', { class: 'kind' }, kindIcon(m.type, m.color_slot), TYPE_LABEL[m.type] || m.type), ' ', statusPill(m) || h('span', { class: 'ep' }, m.epistemic)),
      h('td', { class: 'n' }, fmt(m.usage.returned)),
      h('td', { class: 'n' }, fmt(m.usage.injected)),
      h('td', { class: 'n' }, fmt(m.usage.fetched)),
      h('td', { class: 'n' }, fmt(m.usage.updated)),
      h('td', { class: 'n' }, stale && daysSince(m.created_at) > 30 ? h('span', { class: 'stale-tag' }, ago(last)) : ago(last)),
      h('td', { class: 'n' }, m.criticality.toFixed(2)),
    );
  };

  let cursor = null;
  let shown = 0;
  const load = async (append) => {
    if (ctx.isStale()) return;
    const mine = ++seq;
    const params = { q: f.q, kind: f.kind, status: f.status, sort: f.sort, dir: f.dir, limit: PAGE };
    // `top_scope=`, not `scope=`: the API's `scope=` is a substring filter.
    if (f.top_scope) params.top_scope = f.top_scope;
    if (append && cursor) params.cursor = cursor;
    let page;
    try {
      page = await api.memories(p.id, params);
    } catch (e) {
      if (mine === seq) mount(err, errorBox(e));
      return;
    }
    if (mine !== seq || ctx.isStale()) return;
    mount(err);
    if (!facetsShown) {
      facetsShown = true;
      renderChips(page.facets.scopes);
      const types = page.facets.types;
      mount(
        kindSel,
        h('option', { value: '' }, 'All types'),
        types.map((t) => h('option', { value: t.name, selected: t.name === f.kind }, `${TYPE_LABEL[t.name] || t.name} (${fmt(t.count)})`)),
      );
    }
    const rows = page.items.map(rowEl);
    shown = (append ? shown : 0) + rows.length;
    if (append) tbody.append(...rows);
    else if (rows.length) mount(tbody, rows);
    else if (!page.total) mount(tbody, h('tr', null, h('td', { colspan: COLS.length, class: 'empty' }, page.rebuilding ? 'The index is being rebuilt (a reindex is running); reload in a moment.' : 'This store has no memories yet.')));
    else mount(tbody, h('tr', null, h('td', { colspan: COLS.length, class: 'empty' }, 'No memories match. Clear the filter or turn a top-level scope back on.')));
    cursor = page.next_cursor || null;
    count.textContent = `${fmt(shown)} of ${fmt(page.matched)} shown · ${fmt(page.total)} in store`;
    mount(
      more,
      cursor ? h('button', { type: 'button', class: 'btn', onclick: () => load(true) }, `Load ${fmt(Math.min(PAGE, page.matched - shown))} more`) : null,
      page.usage_lines_skipped ? h('span', { class: 'faint' }, `${fmt(page.usage_lines_skipped)} unreadable usage log lines left out of the counts.`) : null,
    );
  };

  let timer = null;
  search.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (ctx.isStale()) return;
      f.q = search.value.trim();
      writeUrl();
      load(false);
    }, 220);
  });
  kindSel.addEventListener('change', () => {
    f.kind = kindSel.value;
    writeUrl();
    load(false);
  });
  statusSel.addEventListener('change', () => {
    f.status = statusSel.value;
    // Facets count what the status admits, so they change with it.
    facetsShown = false;
    writeUrl();
    load(false);
  });

  header();
  await load(false);
}
