// Graph view: memories are nodes, coloured by top scope and shaped by type;
// relations are the typed edges between them.
//
// **Core and styles.** This module is the core: data loading, the filters
// and their hash state, selection / pin / hover, the keyboard, the details
// card, the relations table, review, counts and the core strings. How the
// graph *looks* is the picked style's (graph/themes/<id>/index.js): its
// layout, memory glyphs, relation drawing, backdrop, legend, CSS under
// `[data-graph-style=<id>]` and optional flavour strings. Core text goes
// through `t(key)` (graph/strings.js), the style's strings first. The core
// builds each memory's `<g class="node">` and each relation's `<g>` (with
// its handlers, aria and hit path) and asks the style for what goes inside.
//
// The server bounds the graph (`/graph?limit=`, "showing N of M"); the
// style's layout (graph/layouts.js) runs in a Web Worker so a large store
// never freezes the page, and is deterministic, so the same store draws the
// same graph.
//
// **Steady graph.** The page asks the server for every edge kind and status
// (only `invalidated` and `top_scope`, which change the memory set, go to the
// server) and lays out that one fixed set; the kind / proposed / rejected
// chips only change which relations are *drawn*, in place, so toggling them
// never moves a memory or the viewport. Layouts are cached per input and
// the viewport per project, so a reload after a review lands where it was.
//
// **Interaction.** Hover (or keyboard focus) shows a small tooltip and
// highlights a memory's relations; a click pins the memory or relation in
// the details card, which is never rebuilt by a hover — review controls in
// it keep their state, and the card links to the memory. Drag pans; the
// wheel zooms only once the graph is engaged (clicked or focused) or with
// Ctrl/⌘ held, so the page still scrolls past it. Keys on the focused graph:
// arrows pan, +/- zoom, 0 fits, ] and [ step through memories, Enter pins,
// Escape unpins. The relations table below lists the proposed relations and
// expands to all of them: the keyboard and screen-reader way to every edge.

import { h, s, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, plural, shortId, label } from '../format.js';
import { badge, scopeName, kindIcon, TYPE_LABEL } from '../scopes.js';
import { KINDS, kindLabel, edgeClass } from '../relations.js';
import { runNamedLayout } from '../graph/layouts.js';
import { along, pathD } from '../graph/geometry.js';
import { makeT } from '../graph/strings.js';
import { STYLES, styleById, storeStyle, useStyle } from '../graph/themes/index.js';
import { projHead, secHead, alertBanner, errorBox } from './common.js';
import { reviewControls } from './review.js';

// ---------------------------------------------------------------------------
// Layout worker and caches
// ---------------------------------------------------------------------------

let worker = null;
let workerBroken = false;
let layoutSeq = 0;
const waiting = new Map();
/** Layout results by input (a few: one per recently viewed graph). */
const layoutCache = new Map();
/** The last viewport per project + layout, so a re-render keeps it. */
const viewports = new Map();
/** Per project: what to pin, and say, when the graph comes back after a review
 * (the page reloads to show the new status): {node: memory id, text}. */
const afterReviewPins = new Map();
/** Set when the Style select changed: the re-render puts focus back on it. */
let refocusStyle = false;
/** The live graph box's resize observer; a new render disconnects the old one. */
let resizeObs = null;

function failWorker() {
  workerBroken = true;
  if (worker) worker.terminate();
  worker = null;
  for (const [, w] of waiting) w.fallback();
  waiting.clear();
}

/** Lay out off the main thread; on the main thread (deferred) if no worker starts. */
function runLayout(input) {
  const key = JSON.stringify(input);
  if (layoutCache.has(key)) return Promise.resolve({ key, lay: layoutCache.get(key) });
  const remember = (lay) => {
    layoutCache.set(key, lay);
    while (layoutCache.size > 4) layoutCache.delete(layoutCache.keys().next().value);
    return { key, lay };
  };
  return new Promise((resolve, reject) => {
    const seq = ++layoutSeq;
    const fallback = () =>
      setTimeout(() => {
        try {
          resolve(runNamedLayout(input));
        } catch (e) {
          reject(e);
        }
      }, 0);
    if (!workerBroken) {
      try {
        if (!worker) {
          worker = new Worker(new URL('../graph/layout-worker.js', import.meta.url), { type: 'module' });
          worker.onmessage = (ev) => {
            const w = waiting.get(ev.data.seq);
            if (!w) return;
            waiting.delete(ev.data.seq);
            if (ev.data.error) w.reject(new Error(ev.data.error));
            else w.resolve(ev.data.result);
          };
          worker.onerror = (ev) => {
            ev.preventDefault();
            failWorker();
          };
        }
        waiting.set(seq, { resolve, reject, fallback });
        worker.postMessage({ seq, input });
        return;
      } catch (_) {
        workerBroken = true;
      }
    }
    fallback();
  }).then(remember);
}

// ---------------------------------------------------------------------------
// Filters (kept in the hash query, so a view can be linked and reloaded)
// ---------------------------------------------------------------------------

function filtersOf(query) {
  const kinds = (query.get('kinds') || '').split(',').filter((k) => KINDS.includes(k));
  return {
    kinds: kinds.length ? kinds : KINDS.slice(),
    proposed: query.get('proposed') !== '0',
    invalidated: query.get('invalidated') === '1',
    rejected: query.get('rejected') === '1',
    top_scope: query.get('top_scope') || '',
    // Only a style named in the hash: none means this browser's stored pick.
    style: styleById(query.get('style')) ? query.get('style') : '',
  };
}

function filterQuery(f) {
  const q = new URLSearchParams();
  if (f.kinds.length !== KINDS.length) q.set('kinds', f.kinds.join(','));
  if (!f.proposed) q.set('proposed', '0');
  if (f.invalidated) q.set('invalidated', '1');
  if (f.rejected) q.set('rejected', '1');
  if (f.top_scope) q.set('top_scope', f.top_scope);
  if (f.style) q.set('style', f.style);
  return q.toString();
}

function hashOf(ctx, f) {
  const qs = filterQuery(f);
  return ctx.href('graph', ctx.project.id) + (qs ? '?' + qs : '');
}

/** Whether relation `e` is drawn under the display filters. */
function shows(f, e) {
  if (!f.kinds.includes(e.kind)) return false;
  if (e.status === 'proposed' && !f.proposed) return false;
  if (e.status === 'rejected' && !f.rejected) return false;
  return true;
}

/** A relation's far-end state, in words (`live` is not shown). */
const FAR_END = { dangling: 'far end deleted', invalidated: 'far end invalidated', unknown: 'far end not checked' };

// ---------------------------------------------------------------------------
// Geometry and text helpers
// ---------------------------------------------------------------------------

const reads = (n) => n.usage.returned + n.usage.injected + n.usage.fetched;
const radius = (n) => 5 + Math.min(6, Math.sqrt(reads(n)) * 0.55);

/**
 * A memory's short label: the summary up to its first clause break, cut on
 * a word boundary. (Titles are not in the index, so the summary is what a
 * graph read can show.)
 */
export function shortLabel(summary, max = 24) {
  let t = String(summary || '').trim();
  const cut = t.search(/[:;—–(]|\.\s|,\s| - /);
  if (cut > 8) t = t.slice(0, cut).trim();
  if (t.length <= max) return t;
  const words = t.slice(0, max).split(/\s+/);
  if (words.length > 1) words.pop();
  return words.join(' ').replace(/[,.:;\s]+$/, '') + '…';
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

export async function renderGraph(main, ctx) {
  if (resizeObs) {
    resizeObs.disconnect();
    resizeObs = null;
  }
  const p = ctx.project;
  const f = filtersOf(ctx.route.query);
  // The render path. Today there is one: every memory the server sent, laid
  // out by the style. Phase E adds a second, core-owned path for a large
  // store (the scope overview: discs and bands, with its own entry in
  // graph/layouts.js and neutral CSS), chosen here before any style hook
  // runs; styles keep drawing only memories and relations.
  const theme = useStyle(f.style);
  const t = makeT(theme);
  if (!p.available) {
    mount(main, projHead('Graph', p.name, [h('span', { class: 'mono' }, p.path)]), h('div', { class: 'note warn' }, `Unavailable: ${p.unavailable_reason || 'no readable store'}.`));
    return;
  }
  mount(main, h('p', { class: 'loading' }, t('loading')));
  let g;
  try {
    // Every kind and status: the display filters apply here (module docs).
    g = await api.graph(p.id, {
      include_proposed: 'true',
      include_rejected: 'true',
      include_invalidated: f.invalidated ? 'true' : 'false',
      top_scope: f.top_scope || undefined,
    });
  } catch (e) {
    if (ctx.isStale()) return;
    mount(main, projHead('Graph', t('title', p.name), []), errorBox(e));
    return;
  }
  if (ctx.isStale()) return;

  // Controls rendered before the layout is ready call through here; they do
  // nothing until the graph exists.
  const ctl = { zoom() {}, fit() {}, pinEdge() {}, redraw() {} };
  const byId = new Map(g.nodes.map((n, i) => [n.id, i]));
  const card = h('aside', { class: 'g-card', 'aria-label': 'Details' });
  const tip = h('div', { class: 'g-tip', role: 'presentation', hidden: true });
  const hint = h('div', { class: 'g-hint', hidden: true }, t('wheelHint'));
  const graphBox = h('div', {
    class: 'graph-box',
    tabindex: '0',
    role: 'group',
    'aria-label': t('graphAria', p.name, g.shown_nodes),
  });
  const legendBox = h('div', { class: 'legend' });
  const tableBox = h('div');
  const alertBox = h('div');
  const notesBox = h('div');
  const filterBox = h('div');

  mount(
    main,
    projHead('Graph', t('title', p.name), t('intro')),
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
          h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Zoom in', onclick: () => ctl.zoom(1 / 1.4) }, '+'),
          h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Zoom out', onclick: () => ctl.zoom(1.4) }, '−'),
          h('button', { type: 'button', class: 'btn sm', onclick: () => ctl.fit() }, 'Fit'),
          h('span', { class: 'faint' }, t('toolsHint')),
          styleControl(),
        ),
        graphBox,
        legendBox,
      ),
      card,
    ),
    tableBox,
  );
  if (refocusStyle) {
    refocusStyle = false;
    const sel = main.querySelector('.g-style select');
    if (sel) sel.focus();
  }

  // The style picker: saved for this browser and named in the hash, so a
  // link copied now shows the same style; re-renders through the hash change
  // (the layout and viewport caches keep a shared layout where it was) and
  // keeps keyboard focus on the select, since a select fires `change` on
  // each arrow key.
  function styleControl() {
    return h(
      'label',
      { class: 'sel g-style' },
      h('span', { class: 'faint' }, t('styleLabel')),
      h(
        'select',
        {
          'aria-label': t('styleAria'),
          onchange: (ev) => {
            storeStyle(ev.target.value);
            refocusStyle = true;
            ctx.navigate(hashOf(ctx, { ...f, style: ev.target.value }), { keepFocus: true });
          },
        },
        STYLES.map((st) => h('option', { value: st.id, selected: st.id === theme.id }, st.name)),
      ),
    );
  }

  // ---- the parts that follow the display filters --------------------------
  const visible = () => g.edges.map((e, k) => k).filter((k) => shows(f, g.edges[k]));

  function setFilters(next) {
    Object.assign(f, next);
    history.replaceState(null, '', hashOf(ctx, f));
    ctl.redraw();
  }

  function renderFilters() {
    const chip = (pressed, content, onclick, title) => h('button', { type: 'button', class: 'chip', 'aria-pressed': String(pressed), onclick, title }, content);
    const kindChips = KINDS.map((k) =>
      chip(f.kinds.includes(k), [theme.edgeSample(k, 'confirmed', 26), kindLabel(k)], () => {
        const on = f.kinds.includes(k);
        // Never filter everything out: the last kind cannot be switched off.
        if (on && f.kinds.length === 1) return;
        setFilters({ kinds: on ? f.kinds.filter((x) => x !== k) : KINDS.filter((x) => x === k || f.kinds.includes(x)) });
      }),
    );
    const toggles = [
      chip(f.proposed, 'Proposed', () => setFilters({ proposed: !f.proposed }), 'Relations awaiting review'),
      chip(f.rejected, 'Rejected', () => setFilters({ rejected: !f.rejected }), 'Relations a reviewer rejected'),
      // These two change the memories, so they reload the graph.
      chip(f.invalidated, 'Invalidated', () => (location.hash = hashOf(ctx, { ...f, invalidated: !f.invalidated })), 'Memories whose validity window closed'),
    ];
    const sel = h(
      'select',
      { 'aria-label': 'Top-level scope shown', onchange: (ev) => (location.hash = hashOf(ctx, { ...f, top_scope: ev.target.value })) },
      h('option', { value: '' }, 'All top-level scopes'),
      g.all_scopes.map((l) => h('option', { value: l.name || '-', selected: (l.name || '-') === f.top_scope }, `${scopeName(l.name)} (${fmt(l.count)})`)),
    );
    // Keep keyboard focus on the chip that was pressed.
    const active = document.activeElement;
    const was = active && filterBox.contains(active) ? [...filterBox.querySelectorAll('button, select')].indexOf(active) : -1;
    mount(
      filterBox,
      h(
        'div',
        { class: 'filt' },
        h('div', { class: 'filt-row', role: 'group', 'aria-label': 'Relation kinds shown' }, h('span', { class: 'eyebrow lab' }, 'Show'), kindChips),
        h('div', { class: 'filt-row', role: 'group', 'aria-label': 'Include' }, h('span', { class: 'eyebrow lab' }, 'Include'), toggles, h('label', { class: 'sel' }, sel)),
      ),
    );
    if (was >= 0) {
      const again = filterBox.querySelectorAll('button, select')[was];
      if (again) again.focus();
    }
  }

  function renderNotes(ks) {
    const shown = ks.map((k) => g.edges[k]);
    const contradictions = ks.filter((k) => g.edges[k].kind === 'contradicts' && g.edges[k].status === 'proposed');
    mount(
      alertBox,
      contradictions.length
        ? alertBanner(
            'Attention',
            `${plural(contradictions.length, 'proposed contradiction')} awaiting review.`,
            null,
            h('button', { type: 'button', class: 'btn sm solid', onclick: () => ctl.pinEdge(contradictions[0], true) }, 'Review first'),
          )
        : null,
    );
    const notes = [];
    if (g.truncated) {
      notes.push(h('div', { class: 'note', role: 'status' }, `Showing ${fmt(g.shown_nodes)} of ${fmt(g.total_nodes)} memories: the most connected first, then the most read. Pick a top-level scope to see the rest.`));
    }
    const dangling = shown.filter((e) => e.state === 'dangling' && (e.dst_store || !byId.has(e.dst))).length;
    const unknown = shown.filter((e) => e.state === 'unknown').length;
    if (dangling || unknown) {
      const bits = [];
      if (dangling) bits.push(t('noteDangling', dangling));
      if (unknown) bits.push(t('noteUnknown', unknown));
      notes.push(h('div', { class: 'note' }, bits.join('; ') + '.'));
    }
    mount(notesBox, notes);
  }

  renderFilters();
  renderNotes(visible());

  if (!g.nodes.length) {
    mount(graphBox, h('p', { class: 'graph-empty muted' }, f.top_scope ? t('emptyScope') : t('empty')));
    mount(card, h('div', { class: 'eyebrow' }, 'Details'), h('p', { class: 'hint' }, 'Nothing to show.'));
    return;
  }
  mount(graphBox, h('p', { class: 'graph-empty muted' }, t('layingOut', g.nodes.length)));

  // Layout input: edge ends as node indices (-1: a stub off the source),
  // and the name of the style's layout (part of the cache key).
  const labels = g.nodes.map((n) => shortLabel(n.summary));
  const boxAspect = Math.min(3, Math.max(0.3, (graphBox.clientHeight || 500) / (graphBox.clientWidth || 800)));
  const res = await runLayout({
    layout: theme.layout.name,
    nodes: g.nodes.map((n, i) => ({ id: n.id, top_scope: n.top_scope || '', slot: n.color_slot ?? null, scope: n.scope || '', degree: n.degree, r: radius(n), label: labels[i], weight: reads(n) })),
    options: theme.layout.options || {},
    edges: g.edges.map((e) => {
      const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
      // A stub's far end is not in the node set, so its scope is unknown
      // (null) unless the server names it.
      return { s: byId.get(e.src), t: ti, kind: e.kind, far_scope: ti >= 0 ? g.nodes[ti].top_scope || '' : (e.far_scope ?? null) };
    }),
    aspect: Math.round(boxAspect * 20) / 20,
    // The box's width in pixels, coarsely (so a resize rarely misses the
    // cache): a layout may size what must stay readable at Fit by it.
    width: Math.round((graphBox.clientWidth || 800) / 40) * 40,
  }).catch((e) => {
    if (!ctx.isStale()) mount(graphBox, errorBox(e));
    return null;
  });
  if (!res || ctx.isStale()) return;
  const lay = res.lay;
  const viewKey = p.id + '|' + theme.layout.name + '|' + res.key.length + '|' + g.nodes.length;

  // ---- static layers ---------------------------------------------------------
  const P = (i) => [lay.pos[2 * i], lay.pos[2 * i + 1]];
  const nodeEls = [];
  const labelEls = [];
  let edgeEls = [];
  let adj = [];

  // The style's backdrop: under the relations and over them (Grid: scope
  // lines and their badges).
  const backdrop = theme.drawBackdrop(g, lay) || {};
  const edgesLayer = s('g', { class: 'edges', 'aria-hidden': 'true' });
  // Labels sit under the memories, so a label never hides one.
  const labelsLayer = s('g', { class: 'labels', 'aria-hidden': 'true' });
  const nodesLayer = s('g', { class: 'nodes' });
  g.nodes.forEach((n, i) => {
    const [x, y] = P(i);
    const r = radius(n);
    const drawn = theme.drawNode(n, r);
    const el = s(
      'g',
      {
        class: ['node', ...(drawn.cls || []), n.degree >= 2 && 'hub', !reads(n) && 'never', n.challenged && 'chal', n.invalidated && 'inv'],
        transform: `translate(${x},${y})`,
        'data-i': i,
        tabindex: '-1',
        role: 'button',
        'aria-label': `${n.summary} (${TYPE_LABEL[n.type] || n.type}, ${plural(n.degree, 'relation')})`,
      },
      // The hover / focus ring is the core's; the rest is the style's.
      s('circle', { class: 'ring', r: r + 6 }),
      drawn.parts,
    );
    el.addEventListener('mouseenter', () => hover({ node: i }));
    el.addEventListener('mouseleave', () => hover(null));
    el.addEventListener('focus', () => hover({ node: i }));
    el.addEventListener('blur', () => hover(null));
    el.addEventListener('click', (ev) => {
      ev.stopPropagation();
      if (!dragged) pin({ node: i }, false);
    });
    el.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter' || ev.key === ' ') {
        ev.preventDefault();
        ev.stopPropagation();
        pin({ node: i }, true);
      }
    });
    nodeEls.push(el);
    nodesLayer.appendChild(el);
    const lp = lay.labels[i];
    const major = n.degree >= 2 || n.proposed > 0 || n.challenged;
    // An unplaced label (no free spot) is drawn hidden and shows only while
    // its memory is highlighted (CSS: .lbl.unplaced).
    const lbl = lp ? s('text', { class: ['lbl', major && 'major', !lp.placed && 'unplaced'], x: lp.x, y: lp.y, 'text-anchor': lp.anchor }, labels[i]) : null;
    labelEls.push(lbl);
    if (lbl) labelsLayer.appendChild(lbl);
  });

  const svg = s(
    'svg',
    { class: 'graph', role: 'group', 'aria-label': 'Memories' },
    s('rect', { class: 'bg', x: -1e5, y: -1e5, width: 2e5, height: 2e5, 'aria-hidden': 'true' }),
    backdrop.below || null,
    edgesLayer,
    backdrop.above || null,
    labelsLayer,
    nodesLayer,
  );
  mount(graphBox, svg, tip, hint);

  // ---- relations (redrawn when the display filters change) ----------------
  function drawEdges() {
    const ks = visible();
    edgeEls = new Array(g.edges.length).fill(null);
    adj = g.nodes.map(() => []);
    const frag = [];
    for (const k of ks) {
      const e = g.edges[k];
      const si = byId.get(e.src);
      const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
      adj[si].push(k);
      if (ti >= 0) adj[ti].push(k);
      const pts = lay.routes[k];
      const drawn = theme.drawEdge(e, pts, { rs: radius(g.nodes[si]) + 4, rt: ti >= 0 ? radius(g.nodes[ti]) + 4 : 9, stub: ti < 0 });
      // The wide transparent hit path is the core's, drawn last (on top).
      const parts = [drawn.parts, s('path', { class: 'hit', d: pathD(pts) })];
      const el = s('g', { class: [...edgeClass(e.kind, e.status, e.state), ...(drawn.cls || [])], 'data-k': k }, parts);
      el.addEventListener('mouseenter', () => hover({ edge: k }));
      el.addEventListener('mouseleave', () => hover(null));
      el.addEventListener('click', (ev) => {
        if (dragged) return;
        ev.stopPropagation();
        pin({ edge: k }, false);
      });
      edgeEls[k] = el;
      frag.push(el);
    }
    mount(edgesLayer, frag);
    for (let i = 0; i < g.nodes.length; i++) {
      nodeEls[i].classList.toggle('hub', adj[i].length >= 2);
    }
    return ks;
  }

  // ---- pan / zoom -----------------------------------------------------------
  let view = viewports.get(viewKey) || null;
  function apply() {
    svg.setAttribute('viewBox', `${view.x.toFixed(1)} ${view.y.toFixed(1)} ${view.w.toFixed(1)} ${view.h.toFixed(1)}`);
    const scale = (graphBox.clientWidth || 1) / view.w;
    svg.classList.toggle('z-near', scale >= 0.8);
    svg.classList.toggle('z-mid', scale >= 0.5 && scale < 0.8);
    viewports.set(viewKey, { ...view });
    while (viewports.size > 8) viewports.delete(viewports.keys().next().value);
  }
  function fit() {
    const b = lay.bounds;
    const aspect = (graphBox.clientHeight || 400) / (graphBox.clientWidth || 600);
    // Keep the box's aspect so a drag maps 1:1 and nothing is distorted.
    let w = b.w;
    let hh = b.w * aspect;
    if (hh < b.h) {
      hh = b.h;
      w = b.h / aspect;
    }
    view = { x: b.x + (b.w - w) / 2, y: b.y + (b.h - hh) / 2, w, h: hh };
    apply();
  }
  function zoom(factor, cx, cy) {
    const px = cx ?? view.x + view.w / 2;
    const py = cy ?? view.y + view.h / 2;
    const w = Math.min(Math.max(view.w * factor, 120), lay.bounds.w * 6 + 2000);
    const k = w / view.w;
    view = { x: px - (px - view.x) * k, y: py - (py - view.y) * k, w, h: view.h * k };
    apply();
  }
  function centerOn(x, y) {
    view = { ...view, x: x - view.w / 2, y: y - view.h / 2 };
    apply();
  }
  const toMap = (clientX, clientY) => {
    const rect = svg.getBoundingClientRect();
    return [view.x + ((clientX - rect.left) / rect.width) * view.w, view.y + ((clientY - rect.top) / rect.height) * view.h];
  };
  const toScreen = (x, y) => {
    const w = graphBox.clientWidth;
    const hh = graphBox.clientHeight;
    return [((x - view.x) / view.w) * w, ((y - view.y) / view.h) * hh];
  };

  let dragged = false;
  const pointers = new Map();
  let start = null;
  // A gesture restarts whenever the number of fingers changes, so a pinch
  // never compounds and lifting one finger does not jump the view.
  function restart() {
    const pts = [...pointers.values()];
    start = { view: { ...view }, x: pts[0] ? pts[0][0] : 0, y: pts[0] ? pts[0][1] : 0, dist: null, n: pts.length };
    if (pts.length >= 2) start.dist = Math.hypot(pts[0][0] - pts[1][0], pts[0][1] - pts[1][1]) || 1;
  }
  svg.addEventListener('pointerdown', (ev) => {
    pointers.set(ev.pointerId, [ev.clientX, ev.clientY]);
    if (pointers.size === 1) dragged = false;
    restart();
    // Engage the graph: focus it so the wheel zooms from now on.
    if (!graphBox.contains(document.activeElement)) graphBox.focus({ preventScroll: true });
  });
  svg.addEventListener('pointermove', (ev) => {
    if (!pointers.has(ev.pointerId) || !start) return;
    pointers.set(ev.pointerId, [ev.clientX, ev.clientY]);
    if (pointers.size !== start.n) restart();
    const rect = svg.getBoundingClientRect();
    if (pointers.size >= 2 && start.dist) {
      const [a, b] = [...pointers.values()];
      const dist = Math.hypot(a[0] - b[0], a[1] - b[1]) || 1;
      const [cx, cy] = toMap((a[0] + b[0]) / 2, (a[1] + b[1]) / 2);
      // Target width relative to the gesture's start, as a factor of now.
      zoom((start.view.w * start.dist) / dist / view.w, cx, cy);
      dragged = true;
      return;
    }
    const dx = ev.clientX - start.x;
    const dy = ev.clientY - start.y;
    if (!dragged && Math.hypot(dx, dy) < 4) return;
    if (!dragged) svg.setPointerCapture(ev.pointerId);
    dragged = true;
    view = { ...view, x: start.view.x - (dx / rect.width) * view.w, y: start.view.y - (dy / rect.height) * view.h };
    apply();
  });
  const end = (ev) => {
    pointers.delete(ev.pointerId);
    if (pointers.size) restart();
    else start = null;
    // `dragged` stays set through the click that follows the pointerup.
    setTimeout(() => {
      if (!pointers.size) dragged = false;
    }, 0);
  };
  svg.addEventListener('pointerup', end);
  svg.addEventListener('pointercancel', end);
  let hintTimer = null;
  graphBox.addEventListener(
    'wheel',
    (ev) => {
      const engaged = graphBox.contains(document.activeElement) || ev.ctrlKey || ev.metaKey;
      if (!engaged) {
        // Let the page scroll; say how to zoom.
        hint.hidden = false;
        clearTimeout(hintTimer);
        hintTimer = setTimeout(() => (hint.hidden = true), 1400);
        return;
      }
      ev.preventDefault();
      hint.hidden = true;
      const [cx, cy] = toMap(ev.clientX, ev.clientY);
      zoom(Math.exp(ev.deltaY * 0.0015), cx, cy);
    },
    { passive: false },
  );
  svg.querySelector('.bg').addEventListener('click', () => {
    if (!dragged) unpin();
  });

  // Keyboard: the graph is one tab stop; ] and [ walk the memories.
  const walk = [...g.nodes.keys()].sort((a, b) => P(a)[1] - P(b)[1] || P(a)[0] - P(b)[0]);
  let walkAt = -1;
  function stepNode(dir) {
    const cur = document.activeElement && document.activeElement.getAttribute ? Number(document.activeElement.getAttribute('data-i')) : NaN;
    if (!Number.isNaN(cur)) walkAt = walk.indexOf(cur);
    walkAt = (walkAt + dir + walk.length) % walk.length;
    const i = walk[walkAt];
    const [x, y] = P(i);
    if (x < view.x || x > view.x + view.w || y < view.y || y > view.y + view.h) centerOn(x, y);
    nodeEls[i].focus({ preventScroll: true });
  }
  graphBox.addEventListener('keydown', (ev) => {
    const onNode = ev.target !== graphBox && ev.target.classList && ev.target.classList.contains('node');
    if (ev.target !== graphBox && !onNode) return;
    const step = ev.shiftKey ? 0.5 : 0.15;
    const keys = {
      ArrowLeft: () => (view.x -= view.w * step),
      ArrowRight: () => (view.x += view.w * step),
      ArrowUp: () => (view.y -= view.h * step),
      ArrowDown: () => (view.y += view.h * step),
    };
    if (keys[ev.key]) {
      keys[ev.key]();
      apply();
    } else if (ev.key === '+' || ev.key === '=') zoom(1 / 1.3);
    else if (ev.key === '-' || ev.key === '_') zoom(1.3);
    else if (ev.key === '0') fit();
    else if (ev.key === ']') stepNode(1);
    else if (ev.key === '[') stepNode(-1);
    else if (ev.key === 'Escape') {
      unpin();
      graphBox.focus();
    } else return;
    ev.preventDefault();
  });
  if (view) apply();
  else fit();
  if (typeof ResizeObserver === 'function') {
    let lastW = graphBox.clientWidth;
    const obs = new ResizeObserver(() => {
      if (!graphBox.isConnected) {
        obs.disconnect();
        if (resizeObs === obs) resizeObs = null;
        return;
      }
      if (Math.abs(graphBox.clientWidth - lastW) > 2) {
        lastW = graphBox.clientWidth;
        fit();
      }
    });
    obs.observe(graphBox);
    resizeObs = obs;
  }

  // ---- highlighting, the tooltip and the card -----------------------------
  let lit = [];
  let pinned = null; // {edge: k} | {node: i}
  function highlight(sel) {
    for (const el of lit) el.classList.remove('on');
    lit = [];
    svg.classList.toggle('has-focus', !!sel);
    if (!sel) return;
    const mark = (el) => {
      if (!el) return;
      el.classList.add('on');
      lit.push(el);
    };
    const markNode = (i) => {
      mark(nodeEls[i]);
      mark(labelEls[i]);
    };
    if (sel.node !== undefined) {
      markNode(sel.node);
      for (const k of adj[sel.node]) {
        mark(edgeEls[k]);
        const e = g.edges[k];
        for (const id of [e.src, e.dst]) if (byId.has(id) && !(id === e.dst && e.dst_store)) markNode(byId.get(id));
      }
    } else {
      const e = g.edges[sel.edge];
      mark(edgeEls[sel.edge]);
      for (const id of [e.src, e.dst]) if (!(id === e.dst && e.dst_store) && byId.has(id)) markNode(byId.get(id));
    }
  }

  /** Hover / focus: highlight and a tooltip. Never touches the card. */
  function hover(sel) {
    highlight(sel || pinned);
    if (!sel) {
      tip.hidden = true;
      return;
    }
    let x;
    let y;
    if (sel.node !== undefined) {
      const n = g.nodes[sel.node];
      [x, y] = P(sel.node);
      mount(tip, h('strong', null, n.summary), h('span', { class: 'muted' }, ` · ${TYPE_LABEL[n.type] || n.type} · ${plural(adj[sel.node].length, 'relation')}`));
    } else {
      const e = g.edges[sel.edge];
      const m = along(lay.routes[sel.edge], 0.5);
      x = m.x;
      y = m.y;
      const name = (id) => (byId.has(id) && !(id === e.dst && e.dst_store) ? shortLabel(g.nodes[byId.get(id)].summary, 32) : shortId(id));
      mount(tip, h('strong', null, kindLabel(e.kind)), h('span', { class: 'muted' }, ` (${label(e.status)}) · ${name(e.src)} ${e.symmetric ? '↔' : '→'} ${name(e.dst)}`));
    }
    const [sx, sy] = toScreen(x, y);
    tip.hidden = false;
    const w = graphBox.clientWidth;
    tip.style.maxWidth = Math.min(320, w - 16) + 'px';
    const tw = tip.offsetWidth;
    tip.style.left = Math.max(8, Math.min(w - tw - 8, sx - tw / 2)) + 'px';
    tip.style.top = Math.max(8, sy - tip.offsetHeight - 14) + 'px';
  }

  function pin(sel, focusCard) {
    pinned = sel;
    if (reviewNote && !(sel.node !== undefined && g.nodes[sel.node].id === reviewNote.node)) reviewNote = null;
    let x;
    let y;
    if (sel.edge !== undefined) {
      const m = along(lay.routes[sel.edge], 0.5);
      x = m.x;
      y = m.y;
    } else [x, y] = P(sel.node);
    const inView = x > view.x && x < view.x + view.w && y > view.y && y < view.y + view.h;
    if (!inView || focusCard) centerOn(x, y);
    tip.hidden = true;
    highlight(pinned);
    renderCard();
    // On a narrow screen the card is below the graph: bring it into view.
    const stacked = window.matchMedia && window.matchMedia('(max-width: 980px)').matches;
    if (stacked) card.scrollIntoView({ block: 'start', behavior: 'smooth' });
    if (focusCard || stacked) {
      const head = card.querySelector('h3');
      if (head) {
        head.setAttribute('tabindex', '-1');
        head.focus({ preventScroll: !stacked && !focusCard });
      }
    }
  }
  function unpin() {
    if (!pinned) return;
    pinned = null;
    reviewNote = null;
    highlight(null);
    renderCard();
  }

  function edgeEnd(e, which) {
    const id = which === 'src' ? e.src : e.dst;
    const i = byId.get(id);
    if (which === 'dst' && e.dst_store) {
      return h('span', null, h('span', { class: 'muted' }, e.state === 'unknown' ? 'In a store that cannot be checked here ' : e.state === 'dangling' ? 'Deleted, in store ' : 'In store '), h('span', { class: 'mono' }, `${shortId(e.dst_store)} · ${shortId(id)}`));
    }
    if (i === undefined) return h('span', null, h('span', { class: 'muted' }, 'Deleted memory '), h('span', { class: 'mono' }, shortId(id)));
    return h('a', { href: ctx.href('detail', p.id, id) }, g.nodes[i].summary);
  }

  // The review's result stays readable across the reload that shows the new
  // status: the reloaded graph pins the edge's source memory and repeats it.
  let reviewNote = null;
  const afterReview = (e) => (out, text) => {
    afterReviewPins.set(p.id, { node: e.src, text });
    setTimeout(() => ctx.refresh(), 900);
  };

  /** The card shows the pinned memory or relation, or the graph summary. */
  function renderCard() {
    const sel = pinned;
    const top = (what) => h('div', { class: 'card-top' }, h('div', { class: 'eyebrow' }, `${what} · pinned`), h('button', { type: 'button', class: 'btn sm ghost', onclick: () => unpin() }, 'Unpin'));
    if (sel && sel.edge !== undefined) {
      const e = g.edges[sel.edge];
      const src = g.nodes[byId.get(e.src)];
      const dstNode = !e.dst_store && byId.has(e.dst) ? g.nodes[byId.get(e.dst)] : null;
      mount(
        card,
        top('Relation'),
        h('h3', null, theme.edgeSample(e.kind, e.status, 40), ' ', kindLabel(e.kind)),
        h(
          'dl',
          null,
          h('dt', null, e.symmetric ? 'Between' : 'From'),
          h('dd', null, edgeEnd(e, 'src')),
          h('dt', null, e.symmetric ? 'And' : 'To'),
          h('dd', null, edgeEnd(e, 'dst')),
          h('dt', null, 'Status'),
          h('dd', null, h('span', { class: ['ep', 'st-' + e.status] }, label(e.status))),
          h('dt', null, 'Origin'),
          h('dd', null, `${e.origin} · ${Number(e.confidence).toFixed(2)}`),
          h('dt', null, 'Since'),
          h('dd', null, ago(e.created_at)),
          e.note ? [h('dt', null, 'Note'), h('dd', { class: 'clamp' }, e.note)] : null,
        ),
        e.state === 'dangling' ? h('p', { class: 'hint' }, 'The far end was deleted. The relation stays in the source file until someone unlinks it.') : null,
        e.state === 'unknown' ? h('p', { class: 'hint' }, 'The far end is in another store that this dashboard could not open, so it is unknown, not missing.') : null,
        e.status === 'proposed'
          ? [h('div', { class: 'eyebrow' }, 'Review'), reviewControls({ ...e, src_summary: src && src.summary, dst_summary: dstNode && dstNode.summary }, ctx, { onDone: afterReview(e) })]
          : null,
      );
      return;
    }
    if (sel && sel.node !== undefined) {
      const n = g.nodes[sel.node];
      const note = reviewNote && reviewNote.node === n.id ? h('p', { class: 'review-status', role: 'status' }, reviewNote.text) : null;
      const items = adj[sel.node].slice(0, 12).map((k) => {
        const e = g.edges[k];
        const oid = e.src === n.id ? e.dst : e.src;
        const other = byId.has(oid) && !(oid === e.dst && e.dst_store) ? g.nodes[byId.get(oid)] : null;
        const dir = e.symmetric ? '' : e.src === n.id ? '→ ' : '← ';
        return h(
          'li',
          null,
          theme.edgeSample(e.kind, e.status, 30),
          h('span', { class: 'muted' }, ` ${dir}${kindLabel(e.kind).toLowerCase()}${e.status === 'proposed' ? ' (proposed)' : e.status === 'rejected' ? ' (rejected)' : ''}`),
          h('br'),
          other ? h('button', { type: 'button', class: 'linkish cn-other', onclick: () => pin({ edge: k }, true) }, other.summary) : h('span', { class: 'faint' }, e.state === 'unknown' ? 'other store (unknown)' : 'deleted memory'),
        );
      });
      mount(
        card,
        top('Memory'),
        note,
        h('div', { class: 'sign sm' }, badge(n.top_scope, n.color_slot), h('span', { class: 'sc' }, n.scope || 'No logical scope')),
        h('h3', null, n.summary),
        h('div', { class: 'mono faint small' }, n.id),
        h(
          'dl',
          null,
          h('dt', null, 'Type'),
          h('dd', null, h('span', { class: 'kind' }, kindIcon(n.type, n.color_slot), TYPE_LABEL[n.type] || n.type)),
          h('dt', null, 'Status'),
          h('dd', null, n.invalidated ? 'invalidated' : label(n.status)),
          h('dt', null, 'Reads'),
          h('dd', null, `${fmt(n.usage.returned)} retrieved · ${fmt(n.usage.injected)} injected · ${fmt(n.usage.fetched)} fetched`),
        ),
        h('a', { class: 'btn solid', href: ctx.href('detail', p.id, n.id) }, 'Open memory →'),
        h('div', { class: 'eyebrow' }, `Relations (${fmt(adj[sel.node].length)})`),
        items.length ? h('ul', null, items) : h('p', { class: 'hint' }, 'No relations shown. Check the filters.'),
      );
      return;
    }
    const ks = visible();
    const shown = ks.map((k) => g.edges[k]);
    const proposed = shown.filter((e) => e.status === 'proposed');
    mount(
      card,
      h('div', { class: 'eyebrow' }, 'Details'),
      h('h3', null, t('cardTitle')),
      h('p', { class: 'hint' }, t('cardHint')),
      h(
        'dl',
        null,
        h('dt', null, 'Top-level scopes'),
        h('dd', null, fmt(g.scopes.filter((l) => l.name).length)),
        h('dt', null, 'Memories'),
        h('dd', null, g.truncated ? `${fmt(g.shown_nodes)} of ${fmt(g.total_nodes)}` : fmt(g.shown_nodes)),
        h('dt', null, 'Relations'),
        h('dd', null, ks.length === g.edges.length ? fmt(ks.length) : `${fmt(ks.length)} of ${fmt(g.edges.length)} (filtered)`),
        h('dt', null, 'Proposed'),
        h('dd', null, `${fmt(proposed.length)}${proposed.some((e) => e.kind === 'contradicts') ? ` (${fmt(proposed.filter((e) => e.kind === 'contradicts').length)} contradictions)` : ''}`),
        g.counts.hidden ? [h('dt', null, 'Not drawn'), h('dd', null, `${plural(g.counts.hidden, 'relation')} to memories outside this view`)] : null,
      ),
    );
  }

  // ---- legend (the style's) -------------------------------------------------
  mount(legendBox, theme.legend(g, t));

  // ---- the relations table: proposed first, the rest on request -----------
  const PAGE = 100;
  let showAll = false;
  let limit = PAGE;
  function renderTable(ks) {
    const rows = ks
      .map((k) => ({ e: g.edges[k], k }))
      .sort((a, b) => (a.e.status === 'proposed' ? 0 : 1) - (b.e.status === 'proposed' ? 0 : 1) || (a.e.kind === 'contradicts' ? 0 : 1) - (b.e.kind === 'contradicts' ? 0 : 1));
    const proposed = rows.filter((r) => r.e.status === 'proposed');
    const listed = showAll ? rows : proposed;
    const toggle = rows.length > proposed.length
      ? h(
          'button',
          {
            type: 'button',
            class: 'btn sm',
            'aria-expanded': String(showAll),
            onclick: () => {
              showAll = !showAll;
              limit = PAGE;
              renderTable(visible());
            },
          },
          showAll ? 'Show proposed only' : `Show all ${fmt(rows.length)} relations`,
        )
      : null;
    const more =
      listed.length > limit
        ? h(
            'button',
            {
              type: 'button',
              class: 'btn sm',
              onclick: (ev) => {
                limit += PAGE;
                const at = limit - PAGE;
                renderTable(visible());
                const row = tableBox.querySelectorAll('tbody tr')[at];
                const b = row && row.querySelector('button');
                if (b) b.focus();
                else ev.target.blur();
              },
            },
            `Show ${fmt(Math.min(PAGE, listed.length - limit))} more`,
          )
        : null;
    mount(
      tableBox,
      secHead(
        'Relations',
        h('span', { class: 'faint num' }, showAll ? plural(rows.length, 'relation') : `${plural(proposed.length, 'proposed relation')} of ${fmt(rows.length)}`),
        toggle,
      ),
      listed.length
        ? h(
            'div',
            { class: 'tbox' },
            h(
              'table',
              { class: 'edges-table' },
              h('caption', { class: 'sr-only' }, showAll ? `Relations in the graph of ${p.name}, proposed ones first.` : `Proposed relations in the graph of ${p.name}.`),
              h(
                'thead',
                null,
                h('tr', null, ['From', 'Relation', 'To', 'Status', 'Origin', 'Confidence', ''].map((col, i) => h('th', { scope: 'col', class: i === 5 ? 'n' : null }, col))),
              ),
              h(
                'tbody',
                null,
                listed.slice(0, limit).map(({ e, k }) =>
                  h(
                    'tr',
                    { class: 'plain-row' },
                    h('td', { class: 't' }, edgeEnd(e, 'src')),
                    h('td', null, h('span', { class: 'kind' }, theme.edgeSample(e.kind, e.status, 30), kindLabel(e.kind))),
                    h('td', { class: 't' }, edgeEnd(e, 'dst')),
                    h('td', null, h('span', { class: ['ep', 'st-' + e.status] }, label(e.status)), e.state !== 'live' ? h('span', { class: 'faint' }, ` · ${FAR_END[e.state] || e.state}`) : null),
                    h('td', null, e.origin),
                    h('td', { class: 'n' }, Number(e.confidence).toFixed(2)),
                    h('td', null, h('button', { type: 'button', class: 'btn sm', onclick: () => pin({ edge: k }, true) }, e.status === 'proposed' ? 'Review' : 'Show in graph')),
                  ),
                ),
              ),
            ),
          )
        : h('p', { class: 'muted' }, rows.length ? 'Nothing awaits review here.' : 'No relations with these filters. Agents add relations with the link tool; NLI and embedding similarity propose them as memories are created.'),
      more,
    );
  }

  function redraw() {
    const ks = drawEdges();
    if (pinned && pinned.edge !== undefined && !edgeEls[pinned.edge]) pinned = null;
    highlight(pinned);
    renderFilters();
    renderNotes(ks);
    renderTable(ks);
    renderCard();
  }

  ctl.zoom = zoom;
  ctl.fit = fit;
  ctl.pinEdge = (k, focus) => pin({ edge: k }, focus);
  ctl.redraw = redraw;
  redraw();
  const back = afterReviewPins.get(p.id);
  if (back) {
    afterReviewPins.delete(p.id);
    if (byId.has(back.node)) {
      reviewNote = back;
      pin({ node: byId.get(back.node) }, true);
    }
  }
}
