// Relations: every relation of a store, as a scope × scope matrix and a
// filterable, paged list that doubles as the review queue.
//
// The matrix counts relations between scopes (top-level, or sub-scopes with
// the toggle): a directed relation in its (source, target) cell, a symmetric
// one in both, so a cell's number is exactly the list a click on it shows.
// A dot marks a cell with contradictions. Clicking a cell filters the list to
// that pair; the server counts the matrix without the pair filter, so the
// matrix never empties under a click. The cells are one keyboard stop: the
// arrow keys move between them (roving tabindex), Enter or Space picks one.
//
// The list comes from GET /relations, proposed relations first, then
// contradictions, then newest. "Load more" follows `next_cursor`, which the
// server binds to the filters, so a page is never one of another list. Each
// row states the relation in words ("<A> supports <B>"), each end's scope,
// its status, origin and confidence, links to both memories and to the
// memory in the graph, and, for a proposed relation, the review controls
// every page uses (views/review.js: shown only with --allow-writes, in-page
// confirm, CSRF, one send per relation). After a review the view reloads
// as many rows as were shown and says what was done on the reviewed
// relation's row (it moves to the confirmed ones), or at the top when that
// row is no longer listed, scrolled into view and focused.
//
// Every filter and the picked cell live in the hash, so a view can be
// linked and reloaded.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { ago, fmt, label, plural, shortId } from '../format.js';
import { badge, scopeName } from '../scopes.js';
import { KINDS, kindLabel } from '../relations.js';
import { currentStyle } from '../graph/themes/index.js';
import { OVERVIEW_MIN } from './graph-overview.js';
import { reviewControls } from './review.js';
import { errorBox, projHead } from './common.js';

const PAGE = 100;

/** A relation in words, read from its source: "A supports B". */
const PHRASE = {
  contradicts: 'contradicts',
  supersedes: 'supersedes',
  supports: 'supports',
  derived_from: 'is derived from',
  related_to: 'is related to',
  duplicates: 'duplicates',
};
const phrase = (kind) => PHRASE[kind] || String(kind).replace(/_/g, ' ');

const STATUS = [
  ['', 'Proposed and confirmed'],
  ['proposed', 'Proposed'],
  ['confirmed', 'Confirmed'],
  ['rejected', 'Rejected'],
  ['confirmed,rejected', 'Confirmed and rejected'],
  ['all', 'Every status'],
];

const FAR_END = { dangling: 'far end deleted', invalidated: 'far end invalidated', unknown: 'far end not checked' };

/**
 * One review result carried across the reload that follows it: the
 * project, the relation's id (to put the note on its row), how many rows
 * were shown (the reload lists as many, so the user keeps their place) and
 * when, so a reload that never rendered (the user left meanwhile) cannot
 * surface it on a later visit.
 */
let reviewNote = null;
const REVIEW_NOTE_MS = 15000;

/** Hash ⇄ filters. `-` names "no scope", as on every endpoint. */
function filtersOf(query) {
  const kinds = (query.get('kind') || '').split(',').filter((k) => KINDS.includes(k));
  return {
    kind: kinds.length === KINDS.length ? [] : kinds,
    status: STATUS.some(([v]) => v === query.get('status')) ? query.get('status') : '',
    q: query.get('q') || '',
    from_scope: query.get('from_scope') || '',
    to_scope: query.get('to_scope') || '',
    touches: query.get('touches') || '',
    subs: query.get('subs') === '1',
    include_invalidated: query.get('include_invalidated') === '1',
  };
}

function hashQuery(f) {
  const u = new URLSearchParams();
  if (f.kind.length) u.set('kind', f.kind.join(','));
  for (const k of ['status', 'q', 'from_scope', 'to_scope', 'touches']) if (f[k]) u.set(k, f[k]);
  if (f.subs) u.set('subs', '1');
  if (f.include_invalidated) u.set('include_invalidated', '1');
  return u.toString();
}

function apiParams(f) {
  return {
    kind: f.kind.join(',') || undefined,
    status: f.status || undefined,
    q: f.q || undefined,
    from_scope: f.from_scope || undefined,
    to_scope: f.to_scope || undefined,
    touches: f.touches || undefined,
    subs: f.subs ? '1' : undefined,
    include_invalidated: f.include_invalidated ? 'true' : undefined,
    limit: PAGE,
  };
}

/** A same-store end's scope at the matrix level (`-`: none). */
const groupKey = (end) => (end && end.group !== undefined ? keyOf(end.group) : null);

/** A cell's number in its 28 px: 1.6k past 999 (the label says it in full). */
const compact = (n) => (n < 1000 ? String(n) : n < 10000 ? (n / 1000).toFixed(1).replace(/\.0$/, '') + 'k' : Math.round(n / 1000) + 'k');

const nameOf = (key) => scopeName(key === '-' ? '' : key);
const keyOf = (name) => name || '-';

/** The picked cell in words, for the filter chip. */
function pairText(f) {
  const kind = f.kind.length === 1 ? f.kind[0] : null;
  const a = f.from_scope && nameOf(f.from_scope);
  const b = f.to_scope && nameOf(f.to_scope);
  if (a && b) return kind ? `${a} ${phrase(kind)} ${b}` : a === b ? `Inside ${a}` : `${a} to ${b}`;
  if (a) return kind ? `${a} ${phrase(kind)} any scope` : `From ${a}`;
  if (b) return kind ? `Any scope ${phrase(kind)} ${b}` : `To ${b}`;
  return '';
}

/** The words for a cell, for its aria-label and title. */
function cellText(a, b, c) {
  const pair = a === b ? `Inside ${scopeName(a)}` : `${scopeName(a)} to ${scopeName(b)}`;
  if (!c) return `${pair}: no relations`;
  const bits = [plural(c.count, 'relation')];
  if (c.contradictions) bits.push(plural(c.contradictions, 'contradiction'));
  if (c.proposed) bits.push(`${fmt(c.proposed)} proposed`);
  return `${pair}: ${bits.join(', ')}`;
}

/** A matrix scope's short header: a sub-scope without its top, which the badge shows. */
function headName(sc, subs) {
  if (!sc.name) return scopeName('');
  return subs && sc.name.includes('.') ? '.' + sc.name.split('.').slice(1).join('.') : sc.name;
}

export async function renderRelations(main, ctx) {
  const p = ctx.project;
  if (!p.available) {
    mount(main, errorBox({ status: 409, message: `${p.name} is unavailable: ${p.unavailable_reason}` }));
    return;
  }
  const f = filtersOf(ctx.route.query);
  const t0 = performance.now();
  // A review's note, taken once: fresh and for this project, or dropped.
  const done = reviewNote && reviewNote.pid === p.id && Date.now() - reviewNote.at < REVIEW_NOTE_MS ? reviewNote : null;
  reviewNote = null;

  const status = h('p', { class: 'cnt num', role: 'status', 'aria-live': 'polite' });
  const noteBox = h('div');
  const matrixBox = h('div', { class: 'mx-wrap' });
  const filterBox = h('div', { class: 'filters rel-filters' });
  const kindBox = h('div', { class: 'chips', role: 'group', 'aria-label': 'Relation kinds shown' });
  // A supersedes relation's target is invalidated by definition, and a
  // relation with an invalidated end is listed only with Invalidated on.
  const supersedesHidden = !f.include_invalidated && f.kind.includes('supersedes');
  const kindNote = supersedesHidden ? h('p', { class: 'faint rel-hint' }, 'Supersedes relations point at an invalidated memory: turn on Invalidated to list them.') : null;
  const listBox = h('ol', { class: 'rel-list', 'aria-label': 'Relations' }, h('li', { class: 'empty' }, 'Loading relations…'));
  const moreBox = h('div', { class: 'more' });
  const err = h('div');

  mount(
    main,
    projHead('Relations', p.name, ['Proposed relations first, then contradictions, then the newest.', 'Pick a cell of the matrix to list the relations between two scopes.']),
    noteBox,
    h(
      'div',
      { class: ['rel-grid', f.subs && 'one'] },
      h(
        'section',
        { class: 'rel-mx', 'aria-label': 'Relations by scope' },
        // Stacked layouts put the list below a tall matrix: one step there.
        h('button', { type: 'button', class: 'btn sm rel-skip', onclick: () => (listBox.querySelector('a, button') || status).focus() }, 'Skip to the relation list'),
        matrixBox,
      ),
      h('section', { class: 'rel-main', 'aria-label': 'Relation list' }, filterBox, kindBox, kindNote, status, err, listBox, moreBox),
    ),
  );
  status.setAttribute('tabindex', '-1');

  // A filter change re-renders the view through the hash; `focus` names the
  // control (its data-focus) that gets focus back afterwards.
  const go = (next, focus) => {
    Object.assign(f, next);
    ctx.state.relFocus = focus;
    ctx.navigate(ctx.href('relations', p.id, hashQuery(f)), { keepFocus: true });
  };
  const refocus = () => {
    const key = ctx.state.relFocus;
    const el = key && main.querySelector(`[data-focus="${key}"]`);
    if (!el) return false;
    ctx.state.relFocus = null;
    el.focus();
    if (el.tagName === 'INPUT') el.setSelectionRange(el.value.length, el.value.length);
    return true;
  };

  // ---- filters ---------------------------------------------------------------
  const search = h('input', { class: 'search', type: 'search', 'data-focus': 'search', placeholder: 'Filter by memory text or note', 'aria-label': 'Filter relations by text', value: f.q });
  let timer = null;
  search.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (ctx.isStale()) return;
      go({ q: search.value.trim() }, 'search');
    }, 250);
  });
  const statusSel = h(
    'select',
    { 'aria-label': 'Status', 'data-focus': 'status', onchange: (ev) => go({ status: ev.target.value }, 'status') },
    STATUS.map(([v, txt]) => h('option', { value: v, selected: v === f.status }, txt)),
  );
  const invalidated = h(
    'button',
    { type: 'button', class: 'chip', 'aria-pressed': String(f.include_invalidated), title: 'Relations touching a memory whose validity window closed', 'data-focus': 'invalidated', onclick: () => go({ include_invalidated: !f.include_invalidated }, 'invalidated') },
    'Invalidated',
  );
  const pair = pairText(f) || (f.touches ? `Touching ${nameOf(f.touches)}` : '');
  const pairChip = pair
    ? h(
        'button',
        {
          type: 'button',
          class: 'chip on rel-pair',
          'aria-label': `Scope filter: ${pair}. Clear it`,
          onclick: () => {
            if (f.from_scope && f.to_scope) ctx.state.relCell = [f.from_scope, f.to_scope];
            go({ from_scope: '', to_scope: '', touches: '' }, 'cell');
          },
        },
        pair,
        h('span', { class: 'x', 'aria-hidden': 'true' }, '×'),
      )
    : null;
  mount(filterBox, search, h('label', { class: 'sel' }, statusSel), invalidated, pairChip);

  const kindOn = new Set(f.kind.length ? f.kind : KINDS);
  mount(
    kindBox,
    KINDS.map((k) =>
      h(
        'button',
        {
          type: 'button',
          class: 'chip',
          'data-focus': 'kind-' + k,
          'aria-pressed': String(kindOn.has(k)),
          onclick: () => {
            if (kindOn.has(k)) kindOn.delete(k);
            else kindOn.add(k);
            if (!kindOn.size) kindOn.add(k); // never "no kind": that lists nothing
            go({ kind: kindOn.size === KINDS.length ? [] : KINDS.filter((x) => kindOn.has(x)) }, 'kind-' + k);
          },
        },
        currentStyle().edgeSample(k, 'confirmed', 22),
        kindLabel(k),
      ),
    ),
  );
  // Controls above the list keep focus while the data loads.
  refocus();

  // ---- data ------------------------------------------------------------------
  let page;
  try {
    // After a review: as many rows as were shown, so the place is kept.
    page = await api.relations(p.id, { ...apiParams(f), limit: done ? Math.min(500, Math.max(PAGE, done.shown)) : PAGE });
  } catch (e) {
    if (!ctx.isStale()) mount(err, errorBox(e));
    return;
  }
  if (ctx.isStale()) return;
  const serverMs = performance.now() - t0;

  renderMatrix(matrixBox, page.matrix, f, go, ctx);

  // Read-only: one note instead of a disabled control box on every row.
  const writes = !!(ctx.state.session && ctx.state.session.allow_writes);
  if (!writes && page.counts.proposed) {
    noteBox.append(h('p', { class: 'note' }, 'Read-only dashboard: restart it with ', h('code', null, 'engramdb dashboard --allow-writes'), ' to confirm or reject proposed relations here.'));
  }
  let shown = 0;
  let cursor = null;
  const addRows = (pg) => {
    const rows = pg.items.map((r) => rowEl(r, ctx, p, page.memories));
    shown += rows.length;
    listBox.append(...rows);
    cursor = pg.next_cursor || null;
    const left = pg.total - shown;
    status.textContent = `${fmt(shown)} of ${plural(pg.total, 'relation')} shown${pg.counts.proposed ? ` · ${fmt(pg.counts.proposed)} proposed` : ''}${pg.counts.contradictions ? ` · ${plural(pg.counts.contradictions, 'contradiction')}` : ''}`;
    mount(
      moreBox,
      cursor
        ? h(
            'button',
            {
              type: 'button',
              class: 'btn',
              onclick: async (ev) => {
                const btn = ev.currentTarget;
                btn.disabled = true;
                let next;
                try {
                  next = await api.relations(p.id, { ...apiParams(f), cursor });
                } catch (e) {
                  btn.disabled = false;
                  mount(err, errorBox(e));
                  return;
                }
                if (ctx.isStale()) return;
                mount(err);
                const first = listBox.children.length;
                addRows(next);
                // Focus the first new row, so the keyboard continues there.
                const row = listBox.children[first];
                const a = row && row.querySelector('a');
                if (a) a.focus();
              },
            },
            `Load ${fmt(Math.min(PAGE, left))} more`,
          )
        : null,
    );
  };
  mount(listBox);
  if (!page.items.length) {
    mount(
      listBox,
      h(
        'li',
        { class: 'empty' },
        page.matrix && page.matrix.relations
          ? `No relation matches these filters. Clear the scope filter, the text, or turn a kind back on.${supersedesHidden ? ' Supersedes relations also need Invalidated on.' : ''}`
          : 'This store has no relations with these filters yet. Agents add relations with the link tool; NLI and embedding similarity propose them as memories are created.',
      ),
    );
    status.textContent = plural(0, 'relation');
  } else addRows(page);
  if (done) {
    // On the reviewed relation's row when it is still among the rows
    // listed; else where that row was (it moved to the confirmed ones,
    // further down), so the user stays at their place in the queue.
    const note = h('p', { class: 'note rel-done', role: 'status', tabindex: '-1' }, done.text);
    const rows = [...listBox.children];
    const row = rows.find((li) => li.dataset.relId === done.id);
    if (row) row.append(note);
    else if (rows[done.index]) listBox.insertBefore(h('li', { class: 'rel-done-at' }, note), rows[done.index]);
    else listBox.append(h('li', { class: 'rel-done-at' }, note));
    note.scrollIntoView({ block: 'center' });
    note.focus({ preventScroll: true });
  } else refocus();

  // Paint marks, for the dashboard benchmark.
  requestAnimationFrame(() => {
    performance.mark('relations:painted');
    const el = document.documentElement;
    el.dataset.relServerMs = String(Math.round(serverMs));
    el.dataset.relPaintMs = String(Math.round(performance.now() - t0));
  });

  function rowEl(r, ctx, p, memories) {
    // In a picked cell, a symmetric relation reads from the row scope's end,
    // as the chip and the matrix ("rows: source scope") do.
    const flip = r.symmetric && !r.dst_store && f.from_scope && groupKey(r.from) !== f.from_scope && groupKey(r.to) === f.from_scope;
    const [left, right] = flip ? ['to', 'from'] : ['from', 'to'];
    const end = (which) => {
      const id = which === 'from' ? r.src : r.dst;
      const e = r[which];
      if (which === 'to' && r.dst_store) {
        return h('span', { class: 'rel-end' }, h('span', { class: 'muted' }, r.state === 'unknown' ? 'a memory in a store that cannot be checked here ' : r.state === 'dangling' ? 'a deleted memory in store ' : 'a memory in store '), h('span', { class: 'mono' }, `${shortId(r.dst_store)} · ${shortId(id)}`));
      }
      if (!e.summary) return h('span', { class: 'rel-end' }, h('span', { class: 'muted' }, 'a deleted memory '), h('span', { class: 'mono' }, shortId(id)));
      return h('a', { class: 'rel-end', href: ctx.href('detail', p.id, id) }, e.summary);
    };
    const scopeOf = (e) =>
      e && e.summary
        ? h('span', { class: 'rel-scope' }, badge(e.top_scope, e.color_slot), h('span', { class: 'mono' }, e.scope || scopeName('')), e.invalidated ? h('span', { class: 'ep invalidated' }, 'invalidated') : null)
        : null;
    // The graph opens on the scope overview above OVERVIEW_MIN memories,
    // so the link opens the source memory's scope there.
    const top = r.from.top_scope || '';
    const gq = new URLSearchParams();
    if (memories > OVERVIEW_MIN) gq.set('top_scope', keyOf(top));
    if (r.status === 'rejected') gq.set('rejected', '1');
    if (r.from.invalidated || r.to.invalidated) gq.set('invalidated', '1');
    gq.set('memory', r.src);
    const review = r.status === 'proposed' && writes ? reviewControls(r, ctx, { onDone: (out, text) => {
      // Named, since the relation may now be off this page.
      const cut = (x) => { const v = String(x || shortId(r.dst)); return v.length > 48 ? v.slice(0, 47) + '…' : v; };
      const index = [...listBox.children].findIndex((li) => li.dataset.relId === r.id);
      reviewNote = { pid: p.id, id: r.id, index, shown, at: Date.now(), text: `“${cut(r.src_summary)}” ${phrase(r.kind)} “${cut(r.dst_summary)}”: ${text}` };
      ctx.refresh({ scroll: false });
    } }) : null;
    return h(
      'li',
      { class: ['rel-row', 'st-' + String(r.status).replace(/[^a-z_]/g, '')], 'data-rel-id': r.id },
      h(
        'p',
        { class: 'rel-says' },
        end(left),
        ' ',
        h('span', { class: 'rel-kind' }, currentStyle().edgeSample(r.kind, r.status, 28), h('b', null, phrase(r.kind))),
        ' ',
        end(right),
      ),
      h(
        'div',
        { class: 'rel-meta' },
        scopeOf(r[left]),
        r.to && r.to.summary ? h('span', { class: 'faint', 'aria-hidden': 'true' }, r.symmetric ? '↔' : '→') : null,
        scopeOf(r[right]),
      ),
      h(
        'div',
        { class: 'rel-meta' },
        h('span', { class: ['ep', 'st-' + r.status] }, label(r.status)),
        r.state !== 'live' ? h('span', { class: 'faint' }, FAR_END[r.state] || r.state) : null,
        h('span', { class: 'faint' }, `${r.origin} · ${Number(r.confidence).toFixed(2)} · ${ago(r.at)}`),
        h('a', { class: 'btn sm', href: ctx.href('graph', p.id, gq.toString()) }, 'Show in graph'),
      ),
      r.note ? h('p', { class: 'rel-note clamp' }, r.note) : null,
      review,
    );
  }
}

/** The matrix, with its level toggle and key. */
function renderMatrix(box, m, f, go, ctx) {
  if (!m) return;
  const toggle = h(
    'div',
    { class: 'rel-level', role: 'group', 'aria-label': 'Matrix scopes' },
    h('span', { class: 'faint' }, 'Scopes:'),
    h('button', { type: 'button', class: 'chip', 'data-focus': 'lvl-top', 'aria-pressed': String(!f.subs), onclick: () => go({ subs: false, from_scope: '', to_scope: '', touches: '' }, 'lvl-top') }, 'Top level'),
    h('button', { type: 'button', class: 'chip', 'data-focus': 'lvl-sub', 'aria-pressed': String(f.subs), onclick: () => go({ subs: true, from_scope: '', to_scope: '', touches: '' }, 'lvl-sub') }, 'Sub-scopes'),
  );
  if (!m.scopes.length) {
    mount(box, toggle, h('p', { class: 'muted' }, m.relations ? 'No relation here has both ends in this store.' : 'No relations to count.'));
    return;
  }
  const cells = new Map(m.cells.map((c) => [c.from + '\u0000' + c.to, c]));
  const max = Math.max(1, ...m.cells.map((c) => c.count));
  const level = (n) => (n ? 1 + Math.min(4, Math.floor(Math.sqrt(n / max) * 5)) : 0);
  const picked = f.from_scope && f.to_scope ? [f.from_scope, f.to_scope] : null;
  const n = m.scopes.length;
  const btns = [];
  let tabAt = 0;
  // The visible header is short (a sub-scope without its top, which the
  // badge shows); assistive tech reads the full name instead.
  const head = (sc) => [h('span', { 'aria-hidden': 'true' }, badge(sc.top, sc.slot), h('span', { class: 'mx-name' }, headName(sc, f.subs))), h('span', { class: 'sr-only' }, scopeName(sc.name))];
  const rows = m.scopes.map((a, i) =>
    h(
      'tr',
      null,
      h('th', { scope: 'row', class: 'mx-rh', title: scopeName(a.name) }, head(a)),
      m.scopes.map((b, j) => {
        const c = cells.get(a.name + '\u0000' + b.name);
        const on = picked && picked[0] === keyOf(a.name) && picked[1] === keyOf(b.name);
        if (on) tabAt = i * n + j;
        const btn = h(
          'button',
          {
            type: 'button',
            class: ['mx-c', 'l' + level(c ? c.count : 0), i === j && 'diag', on && 'on'],
            tabindex: '-1',
            'aria-label': cellText(a.name, b.name, c),
            'aria-pressed': String(!!on),
            'aria-disabled': c ? null : 'true',
            title: cellText(a.name, b.name, c),
            onclick: () => {
              if (!c) return;
              ctx.state.relCell = [keyOf(a.name), keyOf(b.name)];
              if (on) go({ from_scope: '', to_scope: '', touches: '' }, 'cell');
              else go({ from_scope: keyOf(a.name), to_scope: keyOf(b.name), touches: '' }, 'cell');
            },
          },
          c ? compact(c.count) : '',
          c && c.contradictions ? h('i', { class: 'mx-dot', 'aria-hidden': 'true' }) : null,
        );
        btns.push(btn);
        return h('td', null, btn);
      }),
    ),
  );
  // No cell picked: the stop is the cell last picked (so un-picking keeps
  // focus where it was), else the first one with relations.
  if (!picked) {
    const last = ctx.state.relCell;
    const li = last ? m.scopes.findIndex((s) => keyOf(s.name) === last[0]) : -1;
    const lj = last ? m.scopes.findIndex((s) => keyOf(s.name) === last[1]) : -1;
    tabAt = li >= 0 && lj >= 0 ? li * n + lj : Math.max(0, btns.findIndex((b) => !b.hasAttribute('aria-disabled')));
  }
  btns[tabAt].setAttribute('tabindex', '0');
  btns[tabAt].setAttribute('data-focus', 'cell');
  const table = h(
    'table',
    { class: 'mx' },
    h('caption', { class: 'sr-only' }, `Relations between ${f.subs ? 'sub-scopes' : 'top-level scopes'}: rows are the source scope, columns the target. Arrow keys move between cells.`),
    h('thead', null, h('tr', null, h('td'), m.scopes.map((b) => h('th', { scope: 'col', class: 'mx-ch', title: scopeName(b.name) }, h('span', { class: 'mx-v' }, head(b)))))),
    h('tbody', null, rows),
  );
  table.addEventListener('keydown', (ev) => {
    const at = btns.indexOf(document.activeElement);
    if (at < 0) return;
    const i = Math.floor(at / n);
    const j = at % n;
    const moves = { ArrowRight: [0, 1], ArrowLeft: [0, -1], ArrowDown: [1, 0], ArrowUp: [-1, 0] };
    let to = null;
    if (moves[ev.key]) {
      const [di, dj] = moves[ev.key];
      const ni = Math.min(n - 1, Math.max(0, i + di));
      const nj = Math.min(n - 1, Math.max(0, j + dj));
      to = ni * n + nj;
    } else if (ev.key === 'Home') to = ev.ctrlKey ? 0 : i * n;
    else if (ev.key === 'End') to = ev.ctrlKey ? n * n - 1 : i * n + n - 1;
    if (to === null) return;
    ev.preventDefault();
    btns[at].setAttribute('tabindex', '-1');
    btns[at].removeAttribute('data-focus');
    btns[to].setAttribute('tabindex', '0');
    btns[to].setAttribute('data-focus', 'cell');
    btns[to].focus();
  });
  mount(
    box,
    toggle,
    h('div', { class: 'mx-box' }, table),
    h(
      'p',
      { class: 'mx-key faint' },
      h('span', { class: 'mx-ramp', 'aria-hidden': 'true' }, [1, 2, 3, 4, 5].map((l) => h('i', { class: 'l' + l }))),
      'more relations',
      h('span', null, h('i', { class: 'mx-dot', 'aria-hidden': 'true' }), ' has contradictions'),
      h('span', null, 'Rows: source scope. Columns: target scope.'),
      m.unplaced ? h('span', null, `${plural(m.unplaced, 'relation')} to a deleted memory or another store ${m.unplaced === 1 ? 'is' : 'are'} not in the matrix.`) : null,
    ),
  );
}
