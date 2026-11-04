// The details panel of a selected memory: what it is, then its relations
// stated in words, read from the selected memory ("Supported by" means the
// other memory supports this one), so direction never depends on reading a
// taper or an arrow. Core, for every style and every render path (the
// style's graph, the show-all graph and an opened scope).
//
// Order (PANEL_GROUPS): contradictions first, then each directed kind with
// its own direction before the other end's, then the symmetric kinds, then
// any kind this page has no words for, by name. Inside a group, proposed
// relations come first. In an opened scope the relations to other scopes are
// counted per far scope (the server sends tallies, not the relations, so
// their direction is not known here) and listed last, each a link that opens
// that scope.
//
// Each relation row names the other memory (a button that selects it), its
// scope or "same scope", a Proposed / Rejected badge, and a note when the far
// end is not on the graph. A proposed relation is reviewed in its row, with
// the same controls as everywhere (views/review.js: in-page confirm step,
// CSRF, one send per edge); a read-only dashboard shows no controls, one
// note instead.
//
// The panel never changes on hover: only a selection (or a filter change)
// rebuilds it, so a review in progress keeps its state.

import { h } from '../dom.js';
import { fmt, label, shortId } from '../format.js';
import { scopeName, badge, kindIcon, TYPE_LABEL } from '../scopes.js';
import { kindLabel } from '../relations.js';
import { filtered } from './tally.js';

/**
 * The relation groups in panel order: `out` is the string key for the
 * selected memory's own direction (it supports the other), `in` for the
 * other end's (the other supports it). A symmetric kind has one group.
 * Every kind in relations.js's KINDS has an entry (a Rust test checks).
 */
export const PANEL_GROUPS = Object.freeze([
  { kind: 'contradicts', out: 'relContradicts', in: 'relContradicts' },
  { kind: 'supports', out: 'relSupports', in: 'relSupportedBy' },
  { kind: 'supersedes', out: 'relSupersedes', in: 'relSupersededBy' },
  { kind: 'derived_from', out: 'relDerivedFrom', in: 'relSourceOf' },
  { kind: 'duplicates', out: 'relDuplicates', in: 'relDuplicates' },
  { kind: 'related_to', out: 'relRelatedTo', in: 'relRelatedTo' },
]);

const KNOWN = new Map(PANEL_GROUPS.map((gr, i) => [gr.kind, i]));

/** A memory's label in a row: the start of its summary. */
function rowLabel(summary, max = 72) {
  const t = String(summary || '').trim();
  return t.length > max ? t.slice(0, max - 1).trimEnd() + '…' : t;
}

/**
 * Memory `i`'s relations `ks` (indices into `g.edges`) as ordered groups:
 * `[{ kind, incoming, label, items: [{ k, e, j }] }]`, `j` the other end's
 * node index or null when it is not on the graph. Pure, for the panel and
 * its keyboard order.
 */
export function groupRelations(g, byId, i, ks, t) {
  const n = g.nodes[i];
  const groups = new Map();
  for (const k of ks) {
    const e = g.edges[k];
    const incoming = !e.symmetric && e.src !== n.id;
    const oid = e.src === n.id ? e.dst : e.src;
    const j = oid === e.dst && e.dst_store ? null : byId.has(oid) ? byId.get(oid) : null;
    const known = KNOWN.has(e.kind) ? PANEL_GROUPS[KNOWN.get(e.kind)] : null;
    const sym = known ? known.out === known.in : e.symmetric;
    const key = e.kind + (incoming && !sym ? '<' : '>');
    if (!groups.has(key)) {
      const text = known ? t(incoming ? known.in : known.out) : t('relOther', kindLabel(e.kind), incoming);
      groups.set(key, { kind: e.kind, incoming: incoming && !sym, label: text, items: [] });
    }
    groups.get(key).items.push({ k, e, j });
  }
  const rank = (gr) => (KNOWN.has(gr.kind) ? KNOWN.get(gr.kind) * 2 + (gr.incoming ? 1 : 0) : 100);
  const out = [...groups.values()].sort((a, b) => rank(a) - rank(b) || (a.label < b.label ? -1 : a.label > b.label ? 1 : 0));
  const st = (e) => (e.status === 'proposed' ? 0 : e.status === 'rejected' ? 2 : 1);
  const name = (it) => (it.j === null ? '￿' : String(g.nodes[it.j].summary || ''));
  for (const gr of out) gr.items.sort((a, b) => st(a.e) - st(b.e) || (name(a) < name(b) ? -1 : name(a) > name(b) ? 1 : 0) || a.k - b.k);
  return out;
}

/** Why a row's other end is not on the graph (or is but closed), in words; null when it is live here. */
function endNote(e, j, t) {
  if (e.dst_store) return e.state === 'unknown' ? t('panelUnknown') : e.state === 'dangling' ? t('panelDangling') : t('panelOtherStore');
  if (j === null) return e.state === 'unknown' ? t('panelUnknown') : t('panelDangling');
  if (e.state === 'invalidated') return t('panelInvalidated');
  return null;
}

/**
 * The panel for memory `i`. `o`: { g, byId, ks (its visible relations), f
 * (display filters), t, theme, ctx, writes, note (an element or null),
 * onClear(), onSelect(j), onOpenScope(name), review(e) → the review controls
 * for a proposed relation }. Returns { parts, rows }: the panel's children
 * and its rows in panel order, `{ li, node, go }` (`node`: the other memory's
 * index, or null for a row that is not a memory on the graph; `go`: what
 * Enter does, or null).
 */
export function memoryPanel(o) {
  const { g, byId, i, ks, f, t, theme, ctx, writes } = o;
  const n = g.nodes[i];
  const groups = groupRelations(g, byId, i, ks, t);
  const rows = [];

  const groupEls = groups.map((gr) =>
    h(
      'section',
      { class: 'pr-group' },
      h('h4', { class: 'pr-head' }, theme.edgeSample(gr.kind, 'confirmed', 26), h('span', null, gr.label), h('span', { class: 'faint num' }, fmt(gr.items.length))),
      h(
        'ul',
        { class: 'pr-list' },
        gr.items.map(({ k, e, j }) => {
          const other = j === null ? null : g.nodes[j];
          const oid = e.src === n.id ? e.dst : e.src;
          const who = other
            ? h('button', { type: 'button', class: 'linkish pr-other', 'aria-label': t('panelSelectAria', other.summary), onclick: () => o.onSelect(j) }, rowLabel(other.summary))
            : h('span', { class: 'pr-gone mono' }, shortId(oid));
          const where = other ? h('span', { class: 'pr-scope faint' }, (other.scope || '') === (n.scope || '') ? t('panelSameScope') : other.scope || t('panelNoScope')) : null;
          const note = endNote(e, j, t);
          const status =
            e.status === 'proposed'
              ? h('span', { class: ['ep', 'st-proposed'] }, t('panelProposed'))
              : e.status === 'rejected'
                ? h('span', { class: ['ep', 'st-rejected'] }, t('panelRejected'))
                : null;
          const review = e.status === 'proposed' && writes ? o.review(e) : null;
          const li = h(
            'li',
            { class: ['pr-row', e.status === 'proposed' && 'proposed'], 'data-k': k },
            h('div', { class: 'pr-line' }, who, where, status),
            note ? h('div', { class: 'pr-note faint' }, note) : null,
            review,
          );
          rows.push({ li, node: j, go: j === null ? null : () => o.onSelect(j), text: `${gr.label}: ${other ? other.summary : shortId(oid)}${note ? ` (${note})` : ''}` });
          return li;
        }),
      ),
    ),
  );

  // An opened scope: this memory's relations to other scopes, per far scope.
  const far = (g.stubs || [])
    .filter((st) => st.node === n.id)
    .map((st) => ({ st, ft: filtered(st, f) }))
    .filter((x) => x.ft.count > 0)
    .sort((a, b) => b.ft.contradictions - a.ft.contradictions || b.ft.count - a.ft.count || (a.st.scope < b.st.scope ? -1 : 1));
  let farEl = null;
  if (far.length) {
    farEl = h(
      'section',
      { class: 'pr-group' },
      h('h4', { class: 'pr-head' }, h('span', null, t('panelOtherScopes')), h('span', { class: 'faint num' }, fmt(far.reduce((a, x) => a + x.ft.count, 0)))),
      h(
        'ul',
        { class: 'pr-list' },
        far.map(({ st, ft }) => {
          const name = scopeName(st.scope);
          const go = () => o.onOpenScope(st.scope);
          const words = Object.keys(ft.kinds)
            .sort((a, b) => (KNOWN.get(a) ?? 99) - (KNOWN.get(b) ?? 99))
            .map((k) => `${kindLabel(k).toLowerCase()} ${fmt(ft.kinds[k])}`)
            .join(' · ');
          const li = h(
            'li',
            { class: 'pr-row' },
            h(
              'div',
              { class: 'pr-line' },
              h('button', { type: 'button', class: 'linkish pr-other', 'aria-label': t('panelScopeAria', name, ft.count, ft.contradictions), onclick: go }, t('panelScopeRow', name, ft.count)),
              ft.contradictions ? h('span', { class: 'pr-warn' }, `⚠ ${fmt(ft.contradictions)}`) : null,
              ft.proposed ? h('span', { class: ['ep', 'st-proposed'] }, `${t('panelProposed')} ${fmt(ft.proposed)}`) : null,
            ),
            h('div', { class: 'pr-note faint' }, words),
          );
          rows.push({ li, node: null, go, text: `${t('panelOtherScopes')}: ${t('panelScopeRow', name, ft.count)}` });
          return li;
        }),
      ),
    );
  }

  const summary = [
    ...groups.map((gr) => `${gr.label.toLowerCase()} ${fmt(gr.items.length)}`),
    far.length ? `${t('panelOtherScopes').toLowerCase()} ${fmt(far.reduce((a, x) => a + x.ft.count, 0))}` : null,
  ]
    .filter(Boolean)
    .join(' · ');
  const anyProposed = groups.some((gr) => gr.items.some((it) => it.e.status === 'proposed'));

  const parts = [
    h('div', { class: 'card-top' }, h('div', { class: 'eyebrow' }, t('panelEyebrow')), h('button', { type: 'button', class: 'btn sm ghost', 'aria-label': t('panelClearAria'), onclick: () => o.onClear() }, t('panelClear'))),
    o.note,
    h('div', { class: 'sign sm' }, badge(n.top_scope, n.color_slot), h('span', { class: 'sc' }, n.scope || t('panelNoScope'))),
    h('h3', null, n.summary),
    h('div', { class: 'mono faint small' }, n.id),
    h(
      'dl',
      null,
      h('dt', null, t('panelType')),
      h('dd', null, h('span', { class: 'kind' }, kindIcon(n.type, n.color_slot), TYPE_LABEL[n.type] || n.type)),
      h('dt', null, t('panelScope')),
      h('dd', null, n.scope || t('panelNoScope')),
      h('dt', null, t('panelCriticality')),
      h('dd', null, Number(n.criticality ?? 0).toFixed(2)),
      h('dt', null, t('panelStatus')),
      h('dd', null, n.invalidated ? 'invalidated' : label(n.status)),
      h('dt', null, t('panelReads')),
      h('dd', null, t('panelReadsValue', fmt(n.usage.returned), fmt(n.usage.injected), fmt(n.usage.fetched))),
    ),
    h('a', { class: 'btn solid', href: ctx.href('detail', ctx.project.id, n.id) }, t('panelOpen')),
    h('div', { class: 'eyebrow' }, t('panelRelations', fmt(ks.length + far.reduce((a, x) => a + x.ft.count, 0)))),
    summary ? h('p', { class: 'pr-summary' }, summary) : null,
    anyProposed && !writes ? h('p', { class: 'review-note' }, t('panelReadOnly')) : null,
    groupEls.length || farEl ? [groupEls, farEl] : h('p', { class: 'hint' }, t('panelNone')),
    rows.length ? h('p', { class: 'hint pr-keys' }, t('panelKeys')) : null,
  ];
  return { parts, rows };
}
