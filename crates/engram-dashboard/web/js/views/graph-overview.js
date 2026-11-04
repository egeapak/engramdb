// The scope overview: what the graph view shows for a store with more than
// OVERVIEW_MIN memories, before a scope is opened. One disc per top scope
// (area by memories), one band per related scope pair (width by relations),
// and a red ⚠ count on every pair with a contradiction. A core render path,
// the same for every style (styles draw memories and relations, and there
// are none here): neutral words, neutral CSS (`.so-*` in app.css).
//
// Every number comes from the server's totals (`scopes`, `scope_edges`),
// which count every memory the filters admit before the 800-memory limit,
// so the overview of a 10 000-memory store is the whole store. The kind /
// proposed / rejected chips re-count the bands in place (graph/tally.js);
// the discs never move, because the layout's input is the unfiltered pairs.
//
// A band under max(2, 12% of the largest) relations is left out (the note
// under the graph says how many); a pair's ⚠ is always drawn, band or not.
// Click or Enter on a disc opens its scope (`top_scope=` in the hash); the
// pairs table below is the keyboard and screen-reader way to every pair.

import { h, s, mount } from '../dom.js';
import { fmt, plural } from '../format.js';
import { scopeName, slot } from '../scopes.js';
import { KINDS, kindLabel } from '../relations.js';
import { runNamedLayout } from '../graph/layouts.js';
import { filtered, merge, dominant } from '../graph/tally.js';
import { projHead, secHead, alertBanner } from './common.js';

/** Above this many memories (after the status filters), the graph opens on the scope overview. */
export const OVERVIEW_MIN = 150;

/** A band is drawn from max(BAND_MIN, BAND_SHARE × the largest pair) relations. */
const BAND_MIN = 2;
const BAND_SHARE = 0.12;

/** Where along a band its ⚠ may go, best first. */
const SPOTS = [0.5, 0.42, 0.58, 0.34, 0.66, 0.26, 0.74, 0.18, 0.82];

const overviewCache = new Map();

/** The overview layout for this input, cached by input (a few). */
function layoutOf(input) {
  const key = JSON.stringify(input);
  if (!overviewCache.has(key)) {
    overviewCache.set(key, runNamedLayout(input));
    while (overviewCache.size > 4) overviewCache.delete(overviewCache.keys().next().value);
  }
  return overviewCache.get(key);
}

/**
 * Pairs of different top scopes, unordered, each the sum of its server rows
 * (a directed kind has one row per direction): `[{ a, b, tally }]`.
 */
export function scopePairs(g) {
  const pairs = new Map();
  for (const row of g.scope_edges || []) {
    if (row.a === row.b) continue;
    const [a, b] = row.a < row.b ? [row.a, row.b] : [row.b, row.a];
    const key = JSON.stringify([a, b]);
    if (!pairs.has(key)) pairs.set(key, { a, b, tally: { count: 0, kinds: {} } });
    merge(pairs.get(key).tally, row);
  }
  return [...pairs.values()];
}

/**
 * The overview layout of `g`'s scopes for a `width` × `width · aspect` box
 * (also read by an opened scope, for the direction of each far scope).
 * Returns { lay, scopes, pairs, index: name → disc }.
 */
export function overviewLayout(g, width, aspect) {
  const scopes = g.scopes.filter((l) => l.count > 0);
  const index = new Map(scopes.map((l, i) => [l.name, i]));
  const pairs = scopePairs(g).filter((p) => index.has(p.a) && index.has(p.b));
  const input = {
    layout: 'scope-overview',
    nodes: scopes.map((l) => ({ id: l.name, count: l.count, label: `${scopeName(l.name)} · ${fmt(l.count)}` })),
    edges: pairs.map((p) => ({ s: index.get(p.a), t: index.get(p.b), n: p.tally.count })),
    aspect: Math.round(aspect * 20) / 20,
    width: Math.round(width / 40) * 40,
  };
  return { lay: layoutOf(input), scopes, pairs, index };
}

/** The point at `t` on a band's quadratic curve. */
function onCurve(rt, t) {
  const u = 1 - t;
  return [u * u * rt[0][0] + 2 * u * t * rt[1][0] + t * t * rt[2][0], u * u * rt[0][1] + 2 * u * t * rt[1][1] + t * t * rt[2][1]];
}

/** "3 supports, 2 related to" for a filtered tally, largest first. */
function kindWords(ft) {
  return KINDS.filter((k) => ft.kinds[k])
    .sort((a, b) => ft.kinds[b] - ft.kinds[a])
    .map((k) => `${fmt(ft.kinds[k])} ${kindLabel(k).toLowerCase()}`)
    .join(', ');
}

/**
 * Render the overview page into `main`. `env` carries what the graph view
 * owns: { t, f, hashOf, filters(box, onChange), styleControl }.
 */
export function renderOverview(main, ctx, g, env) {
  const { t, f, hashOf } = env;
  const p = ctx.project;
  const open = (name) => (location.hash = hashOf(ctx, { ...f, top_scope: name || '-', all: false }));
  const alertBox = h('div');
  const filterBox = h('div');
  const notesBox = h('div');
  const card = h('aside', { class: 'g-card', 'aria-label': 'Details' });
  const graphBox = h('div', { class: 'graph-box so-box' });
  const legendBox = h('div', { class: 'legend so-legend' });
  const tableBox = h('div');
  const total = g.scopes.reduce((n, l) => n + l.count, 0);
  mount(
    main,
    projHead('Graph', t('title', p.name), t('overviewIntro', total)),
    alertBox,
    filterBox,
    notesBox,
    h(
      'div',
      { class: 'g-grid' },
      h(
        'div',
        { class: 'g-main' },
        h(
          'div',
          { class: 'graph-tools' },
          h('button', { type: 'button', class: 'btn sm', onclick: () => (location.hash = hashOf(ctx, { ...f, all: true })) }, t('showAll')),
          h('span', { class: 'faint' }, t('overviewHint')),
          env.styleControl(),
        ),
        graphBox,
        legendBox,
      ),
      card,
    ),
    tableBox,
  );

  const width = graphBox.clientWidth || 800;
  const aspect = Math.min(3, Math.max(0.3, (graphBox.clientHeight || 500) / width));
  const { lay, scopes, pairs, index } = overviewLayout(g, width, aspect);
  const W = lay.bounds.w;
  const H = lay.bounds.h;
  const P = (i) => [lay.pos[2 * i], lay.pos[2 * i + 1]];

  // ---- static: discs and their names --------------------------------------
  const discEls = [];
  const bandsLayer = s('g', { class: 'so-bands', 'aria-hidden': 'true' });
  const pillsLayer = s('g', { class: 'so-pills', 'aria-hidden': 'true' });
  const discsLayer = s('g', { class: 'so-discs' });
  const inside = new Map();
  for (const row of g.scope_edges || []) if (row.a === row.b) inside.set(row.a, row);
  scopes.forEach((l, i) => {
    const [x, y] = P(i);
    const lp = lay.labels[i];
    const el = s(
      'g',
      { class: 'so-scope', tabindex: '0', role: 'link', 'data-i': i },
      s('circle', { class: ['so-disc', 'stroke-' + slot(l.slot)], cx: x.toFixed(1), cy: y.toFixed(1), r: lay.r[i].toFixed(1) }),
      s('text', { class: ['so-name', !lp.placed && 'unplaced'], x: lp.x.toFixed(1), y: lp.y.toFixed(1), 'text-anchor': lp.anchor }, s('tspan', { class: 'nm' }, scopeName(l.name)), s('tspan', { class: 'ct' }, ` · ${fmt(l.count)}`)),
    );
    el.addEventListener('click', () => open(l.name));
    el.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter' || ev.key === ' ') {
        ev.preventDefault();
        open(l.name);
      }
    });
    el.addEventListener('mouseenter', () => focusOn(i));
    el.addEventListener('mouseleave', () => focusOn(null));
    el.addEventListener('focus', () => focusOn(i));
    el.addEventListener('blur', () => focusOn(null));
    discEls.push(el);
    discsLayer.appendChild(el);
  });
  const svg = s('svg', { class: 'graph so-graph', viewBox: `0 0 ${W} ${H}`, role: 'group', 'aria-label': t('overviewAria', p.name, scopes.length) }, bandsLayer, discsLayer, pillsLayer);
  mount(graphBox, svg);

  let bandEls = [];
  function focusOn(i) {
    svg.classList.toggle('has-focus', i !== null);
    discEls.forEach((el, j) => el.classList.toggle('on', j === i));
    for (const b of bandEls) b.el.classList.toggle('on', i !== null && (b.s === i || b.t === i));
  }

  // ---- the parts that follow the display filters --------------------------
  function paint() {
    const counted = pairs.map((pr, k) => ({ pr, k, ft: filtered(pr.tally, f) })).filter((c) => c.ft.count > 0);
    const most = counted.reduce((n, c) => Math.max(n, c.ft.count), 0);
    const cut = Math.max(BAND_MIN, Math.round(most * BAND_SHARE));
    const drawn = counted.filter((c) => c.ft.count >= cut);
    // A pair left out with a contradiction keeps a hairline, so its ⚠ is
    // never a pill floating between unrelated discs.
    const hairs = counted.filter((c) => c.ft.count < cut && c.ft.contradictions > 0);
    const hidden = counted.length - drawn.length;
    bandEls = [];
    const bands = [];
    // Thin first, so a wide band is never hidden under a thin one.
    for (const c of drawn.slice().sort((a, b) => a.ft.count - b.ft.count)) {
      const rt = lay.routes[c.k];
      const w = 1.5 + 10 * Math.sqrt(c.ft.count / most);
      const dom = dominant(c.ft, KINDS);
      const el = s(
        'path',
        { class: ['so-band', c.ft.contradictions && 'contra'], d: `M${rt[0][0].toFixed(1)},${rt[0][1].toFixed(1)} Q${rt[1][0].toFixed(1)},${rt[1][1].toFixed(1)} ${rt[2][0].toFixed(1)},${rt[2][1].toFixed(1)}`, 'stroke-width': w.toFixed(2), 'data-kind': dom || '' },
        s('title', null, `${scopeName(c.pr.a)} – ${scopeName(c.pr.b)}: ${plural(c.ft.count, 'relation')} (${kindWords(c.ft)})`),
      );
      bands.push(el);
      bandEls.push({ el, s: index.get(c.pr.a), t: index.get(c.pr.b) });
    }
    for (const c of hairs) {
      const rt = lay.routes[c.k];
      const el = s(
        'path',
        { class: 'so-band hair', d: `M${rt[0][0].toFixed(1)},${rt[0][1].toFixed(1)} Q${rt[1][0].toFixed(1)},${rt[1][1].toFixed(1)} ${rt[2][0].toFixed(1)},${rt[2][1].toFixed(1)}`, 'stroke-width': 1 },
        s('title', null, `${scopeName(c.pr.a)} – ${scopeName(c.pr.b)}: ${plural(c.ft.count, 'relation')} (${kindWords(c.ft)})`),
      );
      bands.unshift(el);
      bandEls.push({ el, s: index.get(c.pr.a), t: index.get(c.pr.b) });
    }
    mount(bandsLayer, bands);
    // ⚠ on every pair with a contradiction, drawn band or not, at the first
    // spot along its curve that hits no name and no other ⚠.
    const taken = scopes.map((l, i) => {
      const lp = lay.labels[i];
      if (!lp.placed) return [0, 0, 0, 0];
      const x0 = lp.anchor === 'middle' ? lp.x - lp.w / 2 : lp.anchor === 'end' ? lp.x - lp.w : lp.x;
      return [x0, lp.y - 13, lp.w, 17];
    });
    const pills = [];
    for (const c of counted.filter((q) => q.ft.contradictions > 0).sort((a, b) => b.ft.contradictions - a.ft.contradictions)) {
      const rt = lay.routes[c.k];
      const text = `⚠ ${fmt(c.ft.contradictions)}`;
      const w = text.length * 6.4 + 10;
      let at = null;
      // Free of names, discs and other ⚠ first; failing that, free of other ⚠ only.
      const hitsPill = (box) => taken.some((b) => box[0] < b[0] + b[2] && box[0] + box[2] > b[0] && box[1] < b[1] + b[3] && box[1] + box[3] > b[1]);
      for (const tt of SPOTS) {
        const [x, y] = onCurve(rt, tt);
        const box = [x - w / 2, y - 8, w, 16];
        const overDisc = scopes.some((l, j) => {
          const dx = Math.max(box[0], Math.min(lay.pos[2 * j], box[0] + box[2])) - lay.pos[2 * j];
          const dy = Math.max(box[1], Math.min(lay.pos[2 * j + 1], box[1] + box[3])) - lay.pos[2 * j + 1];
          return dx * dx + dy * dy < lay.r[j] * lay.r[j];
        });
        if (!overDisc && !taken.some((b) => box[0] < b[0] + b[2] && box[0] + box[2] > b[0] && box[1] < b[1] + b[3] && box[1] + box[3] > b[1])) {
          at = [x, y];
          taken.push(box);
          break;
        }
      }
      for (const tt of at ? [] : SPOTS) {
        const [x, y] = onCurve(rt, tt);
        const box = [x - w / 2, y - 8, w, 16];
        if (!hitsPill(box)) {
          at = [x, y];
          taken.push(box);
          break;
        }
      }
      if (!at) at = onCurve(rt, 0.5);
      pills.push(
        s(
          'g',
          { class: 'so-pill', transform: `translate(${at[0].toFixed(1)},${at[1].toFixed(1)})` },
          s('rect', { x: (-w / 2).toFixed(1), y: -8, width: w.toFixed(1), height: 16, rx: 8 }),
          s('text', { y: 4 }, text),
          s('title', null, `${scopeName(c.pr.a)} – ${scopeName(c.pr.b)}: ${plural(c.ft.contradictions, 'contradiction')}`),
        ),
      );
    }
    mount(pillsLayer, pills);

    // Disc names for screen readers: memories, relations out, contradictions.
    const out = scopes.map(() => ({ n: 0, c: 0 }));
    for (const c of counted) {
      for (const nm of [c.pr.a, c.pr.b]) {
        const o = out[index.get(nm)];
        o.n += c.ft.count;
        o.c += c.ft.contradictions;
      }
    }
    scopes.forEach((l, i) => {
      const own = inside.has(l.name) ? filtered(inside.get(l.name), f).count : 0;
      discEls[i].setAttribute(
        'aria-label',
        `${scopeName(l.name)}: ${plural(l.count, 'memory', 'memories')}, ${plural(own, 'relation')} inside, ${plural(out[i].n, 'relation')} to other scopes${out[i].c ? `, ${plural(out[i].c, 'contradiction')}` : ''}. Open this scope.`,
      );
    });

    renderNotes(drawn.length, hidden, cut);
    renderCard(counted);
    renderAlert(counted);
    renderTable(counted);
  }

  function renderNotes(drawn, hidden, cut) {
    mount(
      notesBox,
      h('div', { class: 'note', role: 'status' }, t('overviewNote', total, scopes.length, OVERVIEW_MIN), hidden ? ` ${t('overviewHidden', hidden, cut)}` : ''),
    );
  }

  function renderAlert(counted) {
    let worst = null;
    let n = 0;
    for (const c of counted) {
      const pc = c.pr.tally.kinds.contradicts ? filtered({ kinds: { contradicts: c.pr.tally.kinds.contradicts } }, f).proposed : 0;
      n += pc;
      if (pc && (!worst || pc > worst.pc)) worst = { pc, c };
    }
    for (const [name, row] of inside) {
      const pc = row.kinds.contradicts ? filtered({ kinds: { contradicts: row.kinds.contradicts } }, f).proposed : 0;
      n += pc;
      if (pc && (!worst || pc > worst.pc)) worst = { pc, name };
    }
    const target = worst ? (worst.name !== undefined ? worst.name : worst.c.pr.a) : null;
    mount(
      alertBox,
      n
        ? alertBanner('Attention', `${plural(n, 'proposed contradiction')} awaiting review.`, null, h('button', { type: 'button', class: 'btn sm solid', onclick: () => open(target) }, `Open ${scopeName(target)}`))
        : null,
    );
  }

  function renderCard(counted) {
    const between = counted.reduce((n, c) => n + c.ft.count, 0);
    const contra = counted.reduce((n, c) => n + c.ft.contradictions, 0);
    let within = 0;
    for (const row of inside.values()) within += filtered(row, f).count;
    mount(
      card,
      h('div', { class: 'eyebrow' }, 'Details'),
      h('h3', null, t('overviewCardTitle')),
      h('p', { class: 'hint' }, t('overviewCardHint')),
      h(
        'dl',
        null,
        h('dt', null, 'Top-level scopes'),
        h('dd', null, fmt(scopes.filter((l) => l.name).length)),
        h('dt', null, 'Memories'),
        h('dd', null, fmt(total)),
        h('dt', null, 'Inside scopes'),
        h('dd', null, plural(within, 'relation')),
        h('dt', null, 'Between scopes'),
        h('dd', null, `${plural(between, 'relation')}${contra ? ` (${plural(contra, 'contradiction')})` : ''}`),
      ),
    );
  }

  function renderTable(counted) {
    const rows = counted.slice().sort((a, b) => b.ft.contradictions - a.ft.contradictions || b.ft.count - a.ft.count || (a.pr.a + '\0' + a.pr.b < b.pr.a + '\0' + b.pr.b ? -1 : 1));
    const LIMIT = 50;
    const scopeBtn = (name) => h('button', { type: 'button', class: 'linkish', onclick: () => open(name) }, scopeName(name));
    mount(
      tableBox,
      secHead(t('overviewPairs'), h('span', { class: 'faint num' }, plural(rows.length, 'scope pair'))),
      rows.length
        ? h(
            'div',
            { class: 'tbox' },
            h(
              'table',
              { class: 'edges-table so-table' },
              h('caption', { class: 'sr-only' }, `Relations between the top-level scopes of ${p.name}, contradictions first.`),
              h('thead', null, h('tr', null, ['Scope', 'Scope', 'Relations', 'Contradictions', 'Proposed', 'Kinds'].map((col, i) => h('th', { scope: 'col', class: i >= 2 && i <= 4 ? 'n' : null }, col)))),
              h(
                'tbody',
                null,
                rows.slice(0, LIMIT).map((c) =>
                  h(
                    'tr',
                    { class: 'plain-row' },
                    h('td', null, scopeBtn(c.pr.a)),
                    h('td', null, scopeBtn(c.pr.b)),
                    h('td', { class: 'n' }, fmt(c.ft.count)),
                    h('td', { class: 'n' }, c.ft.contradictions ? h('b', { class: 'so-contra' }, `⚠ ${fmt(c.ft.contradictions)}`) : '0'),
                    h('td', { class: 'n' }, fmt(c.ft.proposed)),
                    h('td', null, kindWords(c.ft)),
                  ),
                ),
              ),
            ),
          )
        : h('p', { class: 'muted' }, t('overviewNoPairs')),
      rows.length > LIMIT ? h('p', { class: 'faint' }, t('overviewMorePairs', rows.length - LIMIT)) : null,
    );
  }

  mount(
    legendBox,
    h(
      'div',
      null,
      h('h4', null, t('legendOverview')),
      h(
        'ul',
        null,
        h('li', null, s('svg', { width: 34, height: 22, viewBox: '0 0 34 22', 'aria-hidden': 'true', class: 'so-lg' }, s('circle', { class: 'so-disc stroke-none', cx: 11, cy: 11, r: 9 })), t('legendDisc')),
        h('li', null, s('svg', { width: 34, height: 22, viewBox: '0 0 34 22', 'aria-hidden': 'true', class: 'so-lg' }, s('path', { class: 'so-band', d: 'M2,15 Q17,5 32,15', 'stroke-width': 5 })), t('legendBand')),
        h('li', null, s('svg', { width: 34, height: 22, viewBox: '-17 -11 34 22', 'aria-hidden': 'true', class: 'so-lg' }, s('g', { class: 'so-pill' }, s('rect', { x: -15, y: -8, width: 30, height: 16, rx: 8 }), s('text', { y: 4 }, '⚠ 2'))), t('legendPill')),
      ),
    ),
  );

  env.filters(filterBox, paint);
  paint();
}
