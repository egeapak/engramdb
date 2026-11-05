// Graph view: memories are nodes, shaped by type and grouped by top scope;
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
// focuses a memory: `.has-focus` on the <svg> fades everything (one class,
// so a hover never walks every memory), `.on` lights the memory, its
// relations and its neighbours, and `.nb` marks the neighbours (their labels
// show even where the layout placed none). Only those elements are touched,
// through the adjacency drawEdges keeps; each style's CSS says what faded and
// lit look like. A click selects the memory (or pins a relation): the
// details panel (../graph/details.js) replaces the summary card and states
// its relations in words, proposed ones with their review controls. The
// panel is never rebuilt by a hover, so a review in it keeps its state.
// Drag pans; the wheel zooms only once the graph is engaged (clicked or
// focused) or with Ctrl/⌘ held, so the page still scrolls past it. Keys on
// the focused graph: arrows pan, +/- zoom, 0 fits, ] and [ step through the
// memories — or, with a memory selected, through its panel's rows, focusing
// each neighbour (on a stacked phone layout focus stays on the row and the
// neighbour is ringed, `.cur`), each row said in words through a live
// region — Enter selects the focused memory (or opens the focused row's
// scope), Escape clears the selection and returns focus to the graph (from
// the panel too). A focus re-places the lit labels among themselves
// (`relabel`, O(degree)) so revealed labels never overlap; one with no spot
// is hidden and named in the tooltip, which sits in the box corner away
// from the memory. After a review the first end of the relation still drawn
// is selected again (the reviewer's memory, else — when it was superseded —
// the other), with the result read out. The relations table below lists the proposed relations and
// expands to all of them: the keyboard and screen-reader way to every edge.
//
// **Three render paths.** A store with more than OVERVIEW_MIN memories (after
// the status filters) opens on the scope overview (./graph-overview.js: one
// disc per top scope, bands between them), unless the hash says `all=1`
// ("Show all memories", still bounded by the server's limit). A scope opened
// from it (`top_scope=<name>`) is laid out by the core scope-force layout
// whatever the style, sub-scopes as regions, and its relations to other
// scopes are faint stubs ending in one tag per far scope (../graph/opened.js).
// Everything else — every store of OVERVIEW_MIN memories or fewer, and the
// show-all graph — is the style's own full graph, drawn as it always was.
// On a graph of more than OVERVIEW_MIN memories zoomed to 3× Fit or more,
// only relations with both ends on screen are drawn whole, the rest as
// short stubs (recomputed when a pan or zoom settles, never per frame).

import { h, s, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, plural, shortId, label } from '../format.js';
import { badge, scopeName, kindIcon, TYPE_LABEL } from '../scopes.js';
import { KINDS, kindLabel, edgeClass } from '../relations.js';
import { runNamedLayout, usesWidth } from '../graph/layouts.js';
import { isFor, decode, serverKey, SERVER_LAYOUTS } from '../graph/server-layout.js';
import { shortLabel, forceInput, roundAspect, roundWidth } from '../graph/input.js';
import { along, pathD } from '../graph/geometry.js';
import { reads, radius } from '../graph/radius.js';
import { makeT } from '../graph/strings.js';
import { STYLES, styleById, storeStyle, useStyle } from '../graph/themes/index.js';
import { filtered } from '../graph/tally.js';
import { drawRegions, placeTags } from '../graph/opened.js';
import { memoryPanel } from '../graph/details.js';
import { placeLabels } from '../graph/labels.js';
import { projHead, secHead, alertBanner, errorBox } from './common.js';
import { reviewControls } from './review.js';
import { OVERVIEW_MIN, renderOverview, overviewLayout } from './graph-overview.js';

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
/** FNV-1a over a layout's cache key: the zoom kept per laid-out graph. */
function hashKey(str) {
  let a = 0x811c9dc5;
  let b = 0x01000193 ^ str.length;
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    a = Math.imul(a ^ c, 0x01000193);
    b = Math.imul(b ^ c, 0x5bd1e995);
  }
  return (a >>> 0).toString(16) + (b >>> 0).toString(16) + '.' + str.length;
}

/** The layout worker, started on first use (null when none can start). */
function ensureWorker() {
  if (workerBroken) return null;
  if (worker) return worker;
  try {
    worker = new Worker(new URL('../graph/layout-worker.js', import.meta.url), { type: 'module' });
  } catch (_) {
    workerBroken = true;
    return null;
  }
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
  return worker;
}

/**
 * Start the layout worker ahead of need (the overview does not use it, the
 * scope it opens will): it loads its modules now, and nothing is laid out
 * or cached.
 */
function warmWorker() {
  ensureWorker();
}

/**
 * Roughly how much of the drawn graph page is not the graph box (heading,
 * filters, tools, legend): with the box it decides whether the page will
 * scroll, so the probe below takes the width a scrollbar leaves.
 */
const PAGE_EXTRA = 300;

/**
 * The box a graph view will draw in, measured before its data arrives (the
 * server lays out for the box's shape): a hidden copy of the view's grid in
 * `main`, inside a zero-height wrapper so the loading page never scrolls.
 * The page's scrollbar is set, for the measurement only, to what the drawn
 * page will have (it scrolls when the box plus PAGE_EXTRA is taller than
 * the window), since a classic scrollbar narrows the box. A wrong guess
 * only costs speed: the drawn box then misses the server's layout and the
 * worker lays out. Returns { aspect, width } rounded as the input rounds them.
 */
export function measureBox(main, loadingEl) {
  const box = h('div', { class: 'graph-box' });
  mount(main, loadingEl, h('div', { class: 'g-probe', 'aria-hidden': 'true' }, h('div', { class: 'g-grid' }, h('div', { class: 'g-main' }, box), h('aside', { class: 'g-card' }))));
  const root = document.documentElement;
  const scrolls = root.scrollHeight > root.clientHeight;
  const willScroll = (box.clientHeight || 500) + PAGE_EXTRA > window.innerHeight;
  const before = root.style.overflowY;
  if (scrolls !== willScroll) root.style.overflowY = willScroll ? 'scroll' : 'hidden';
  const w = box.clientWidth || 800;
  const ht = box.clientHeight || 500;
  root.style.overflowY = before;
  return { aspect: roundAspect(ht / w), width: roundWidth(w) };
}

/**
 * The layout of `input`: from the page's cache, the server's layout of this
 * very input (`server`: { L, groups }, graph/server-layout.js), else the
 * worker. A server layout is cached under its digest (`serverKey`), so the
 * page never stringifies a large input it did not have to lay out.
 */
function runLayout(input, server) {
  const remember = (key, lay, source) => {
    layoutCache.set(key, lay);
    while (layoutCache.size > 4) layoutCache.delete(layoutCache.keys().next().value);
    return { key, lay, source };
  };
  const sKey = server && serverKey(server.L);
  if (sKey && isFor(server.L, input, server.groups)) {
    if (layoutCache.has(sKey)) return Promise.resolve({ key: sKey, lay: layoutCache.get(sKey), source: 'cache' });
    const lay = decode(server.L, input);
    if (lay) return Promise.resolve(remember(sKey, lay, 'server'));
  }
  const key = JSON.stringify(input);
  if (layoutCache.has(key)) return Promise.resolve({ key, lay: layoutCache.get(key), source: 'cache' });
  let source = 'worker';
  return new Promise((resolve, reject) => {
    const seq = ++layoutSeq;
    const fallback = () =>
      setTimeout(() => {
        source = 'main';
        try {
          resolve(runNamedLayout(input));
        } catch (e) {
          reject(e);
        }
      }, 0);
    const wk = ensureWorker();
    if (wk) {
      try {
        waiting.set(seq, { resolve, reject, fallback });
        wk.postMessage({ seq, input });
        return;
      } catch (_) {
        waiting.delete(seq);
        workerBroken = true;
      }
    }
    fallback();
  }).then((lay) => remember(key, lay, source));
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
    // "Show all memories": the full graph even above OVERVIEW_MIN.
    all: query.get('all') === '1',
    // Only a style named in the hash: none means this browser's stored pick.
    style: styleById(query.get('style')) ? query.get('style') : '',
    // A memory to select once drawn (a link from the Relations tab); taken
    // out of the hash once used, so a later click is not undone by it.
    memory: query.get('memory') || '',
  };
}

function filterQuery(f) {
  const q = new URLSearchParams();
  if (f.kinds.length !== KINDS.length) q.set('kinds', f.kinds.join(','));
  if (!f.proposed) q.set('proposed', '0');
  if (f.invalidated) q.set('invalidated', '1');
  if (f.rejected) q.set('rejected', '1');
  if (f.top_scope) q.set('top_scope', f.top_scope);
  if (f.all) q.set('all', '1');
  if (f.style) q.set('style', f.style);
  if (f.memory) q.set('memory', f.memory);
  return q.toString();
}

/** The Relations tab's hash query for the graph's filters. */
function relationsQuery(f) {
  const q = new URLSearchParams();
  if (f.kinds.length !== KINDS.length) q.set('kind', f.kinds.join(','));
  if (f.rejected && !f.proposed) q.set('status', 'confirmed,rejected');
  else if (f.rejected) q.set('status', 'all');
  else if (!f.proposed) q.set('status', 'confirmed');
  if (f.invalidated) q.set('include_invalidated', '1');
  if (f.top_scope && !f.top_scope.includes(',')) q.set('touches', f.top_scope);
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

/** From this zoom (× the Fit scale) a big graph draws only on-screen relations whole. */
const CUT_ZOOM = 3;

/** Above this many far-scope stubs, they are drawn faint until highlighted. */
const STUBS_FAINT = 50;

/** A far scope's tag text, for sizing ("storage · 12  ⚠ 2"). */
function tagText(name, count, contradictions) {
  return `${scopeName(name)} · ${fmt(count)}${contradictions ? `  ⚠ ${fmt(contradictions)}` : ''}`;
}

/** A relation's far-end state, in words (`live` is not shown). */
const FAR_END = { dangling: 'far end deleted', invalidated: 'far end invalidated', unknown: 'far end not checked' };

// ---------------------------------------------------------------------------
// Geometry and text helpers
// ---------------------------------------------------------------------------

/**
 * The filter rows (kinds, proposed / rejected / invalidated, the top-scope
 * picker) into `filterBox`; shared by the graph and the scope overview.
 * `setFilters(next)` applies a display filter in place; the invalidated chip
 * and the picker change the memory set, so they go through the hash.
 */
function buildFilters(filterBox, ctx, f, g, theme, setFilters) {
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
    { 'aria-label': 'Top-level scope shown', onchange: (ev) => (location.hash = hashOf(ctx, { ...f, top_scope: ev.target.value, all: ev.target.value ? f.all : false })) },
    h('option', { value: '' }, 'All top-level scopes'),
    g.scopes.map((l) => h('option', { value: l.name || '-', selected: (l.name || '-') === f.top_scope }, `${scopeName(l.name)} (${fmt(l.count)})`)),
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
  // The render path (module docs), chosen before any style hook runs: the
  // overview asks with `summary_over=OVERVIEW_MIN`, so a store at or under
  // it comes back whole (and is drawn exactly as before) while a larger one
  // sends its totals only, in the same one request.
  const wantOverview = !f.top_scope && !f.all;
  const theme = useStyle(f.style);
  const t = makeT(theme);
  if (!p.available) {
    mount(main, projHead('Graph', p.name, [h('span', { class: 'mono' }, p.path)]), h('div', { class: 'note warn' }, `Unavailable: ${p.unavailable_reason || 'no readable store'}.`));
    return;
  }
  // Ask the server to lay out what it can (graph/server-layout.js): the
  // scope overview, scope-force when the style uses it with no options of
  // its own, and an opened scope (scope-force by sub-scope) when one scope
  // is picked: the server knows the store's size, so it lays that out only
  // when the store has an overview (more than OVERVIEW_MIN memories, as
  // `opened` below decides), else the style's own when it asked. It answers
  // with the one that fits its response.
  const box = measureBox(main, h('p', { class: 'loading' }, t('loading')));
  const singleScope = f.top_scope && !f.top_scope.includes(',');
  const ownForce = theme.layout.name === 'scope-force' && !Object.keys(theme.layout.options || {}).length;
  const asked = [wantOverview && 'scope-overview', ownForce && 'scope-force', singleScope && 'opened'].filter((n) => n && (n === 'opened' || SERVER_LAYOUTS[n]));
  let g;
  try {
    // Every kind and status: the display filters apply here (module docs).
    g = await api.graph(p.id, {
      include_proposed: 'true',
      include_rejected: 'true',
      include_invalidated: f.invalidated ? 'true' : 'false',
      top_scope: f.top_scope || undefined,
      summary_over: wantOverview ? String(OVERVIEW_MIN) : undefined,
      layout: asked.length ? asked.join(',') : undefined,
      layout_aspect: asked.length ? String(box.aspect) : undefined,
      layout_width: asked.length ? String(box.width) : undefined,
      layout_open_over: singleScope ? String(OVERVIEW_MIN) : undefined,
    });
  } catch (e) {
    if (ctx.isStale()) return;
    mount(main, projHead('Graph', t('title', p.name), []), errorBox(e));
    return;
  }
  if (ctx.isStale()) return;
  // Timing marks for the browser bench (bench/browser/); a mark is a few
  // hundred nanoseconds and nothing reads them otherwise.
  performance.mark('graph:data');

  if (g.summary) {
    // The overview draws no memory to select.
    f.memory = '';
    renderOverview(main, ctx, g, {
      t,
      f,
      hashOf,
      styleControl,
      filters: (box, onChange) => {
        const apply = (next) => {
          Object.assign(f, next);
          history.replaceState(null, '', hashOf(ctx, f));
          buildFilters(box, ctx, f, g, theme, apply);
          onChange();
        };
        buildFilters(box, ctx, f, g, theme, apply);
      },
    });
    if (refocusStyle) {
      refocusStyle = false;
      const sel = main.querySelector('.g-style select');
      if (sel) sel.focus();
    }
    warmWorker();
    performance.mark('graph:overview-built');
    return;
  }
  // A big graph (an opened scope, or show-all past OVERVIEW_MIN) draws only
  // on-screen relations whole once zoomed in (module docs).
  const big = g.nodes.length > OVERVIEW_MIN;
  // The opened-scope path is the overview's drill-in, so only a store that
  // has an overview gets it: a smaller one filtered by the picker keeps the
  // style's own graph (Grid stays Grid).
  const storeTotal = g.scopes.reduce((n, l) => n + l.count, 0);
  const opened = f.top_scope && !f.top_scope.includes(',') && storeTotal > OVERVIEW_MIN ? f.top_scope : null;
  // Show-all's way back sits in the tools row instead (one button), so the
  // full graph's box stays where it always was.
  // Back to the overview keeps focus for the overview to put on the scope
  // that was opened (views/graph-overview.js), not on the page heading.
  function backTo(hash) {
    return (ev) => {
      if (ev.button || ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.altKey) return;
      ev.preventDefault();
      ctx.navigate(hash, { keepFocus: true });
    };
  }
  const backToOverview = !opened && f.all && storeTotal > OVERVIEW_MIN ? h('a', { class: 'btn sm', href: hashOf(ctx, { ...f, all: false }), onclick: backTo(hashOf(ctx, { ...f, all: false })) }, t('crumbAll')) : null;
  const crumbs =
    opened
      ? h(
          'nav',
          { class: 'g-crumbs', 'aria-label': t('crumbsAria') },
          h('a', { href: hashOf(ctx, { ...f, top_scope: '', all: false }), onclick: backTo(hashOf(ctx, { ...f, top_scope: '', all: false })) }, t('crumbAll')),
          h('span', { class: 'sep', 'aria-hidden': 'true' }, ' › '),
          h('span', { 'aria-current': 'page' }, scopeName(opened === '-' ? '' : opened)),
        )
      : null;

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
        crumbs,
        h(
          'div',
          { class: 'graph-tools' },
          h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Zoom in', onclick: () => ctl.zoom(1 / 1.4) }, '+'),
          h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Zoom out', onclick: () => ctl.zoom(1.4) }, '−'),
          h('button', { type: 'button', class: 'btn sm', onclick: () => ctl.fit() }, 'Fit'),
          backToOverview,
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
    buildFilters(filterBox, ctx, f, g, theme, setFilters);
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

  // Layout input (graph/input.js, which the server mirrors): edge ends as
  // node indices (-1: a stub off the source), and the name of the style's
  // layout (part of the cache key). An opened scope: scope-force whatever
  // the style, each sub-scope a region (its key stands in for the top scope
  // the layout groups by). The box's width goes in only for a layout that
  // sizes what must stay readable at Fit by it (coarsely, so a resize
  // rarely misses the cache): any other would lay out again on every resize
  // for nothing.
  const labels = g.nodes.map((n) => shortLabel(n.summary));
  const layoutName = opened ? 'scope-force' : theme.layout.name;
  // The box's size, read once here, while the page is cheap to lay out (no
  // graph in it yet), and kept current by the resize observer below: every
  // read of it while the graph is built would force a style and layout pass
  // over thousands of new SVG elements, and the paint does that pass anyway.
  const boxSize = { w: graphBox.clientWidth, h: graphBox.clientHeight };
  const { input, groupKeys } = forceInput(g, {
    layout: layoutName,
    options: opened ? {} : theme.layout.options,
    opened: !!opened,
    aspect: (boxSize.h || 500) / (boxSize.w || 800),
    width: usesWidth(layoutName) ? boxSize.w || 800 : null,
    labels,
  });
  // The server's layout says which grouping it is for (`groups`).
  const serverLay = g.layout ? { L: g.layout, groups: opened ? 'sub' : 'top' } : null;
  const res = await runLayout(input, serverLay).catch((e) => {
    if (!ctx.isStale()) mount(graphBox, errorBox(e));
    return null;
  });
  if (!res || ctx.isStale()) return;
  // `source`: where the layout came from (worker, main thread, the page's
  // cache, or the server), for the browser bench.
  performance.mark('graph:layout', { detail: { source: res.source } });
  const lay = res.lay;
  const viewKey = p.id + '|' + hashKey(res.key);

  // ---- static layers ---------------------------------------------------------
  const P = (i) => [lay.pos[2 * i], lay.pos[2 * i + 1]];
  const nodeEls = [];
  // Per memory: its label element, `undefined` while not built yet (an
  // unplaced one), null for none.
  const labelEls = [];
  function makeLabel(i) {
    const n = g.nodes[i];
    const lp = lay.labels[i];
    const major = n.degree >= 2 || n.proposed > 0 || n.challenged;
    return s('text', { class: ['lbl', major && 'major', !lp.placed && 'unplaced'], x: lp.x, y: lp.y, 'text-anchor': lp.anchor }, labels[i]);
  }
  /** Memory `i`'s label element, built (in index order in its layer) on first need. */
  function labelEl(i) {
    if (labelEls[i] !== undefined) return labelEls[i];
    const el = makeLabel(i);
    let j = i + 1;
    while (j < labelEls.length && !labelEls[j]) j++;
    labelsLayer.insertBefore(el, j < labelEls.length ? labelEls[j] : null);
    labelEls[i] = el;
    return el;
  }
  let edgeEls = [];
  let adj = [];

  // The style's backdrop: under the relations and over them (Grid: scope
  // lines and their badges). An opened scope's regions are its sub-scopes:
  // a style laid out by scope-force draws them as its own regions (it reads
  // each node's group as its top scope); any other gets the core's neutral
  // ones, since its backdrop expects its own layout.
  const backdrop = !opened
    ? theme.drawBackdrop(g, lay) || {}
    : theme.layout.name === layoutName
      ? theme.drawBackdrop({ ...g, nodes: g.nodes.map((n, i) => ({ ...n, top_scope: groupKeys[i] })) }, lay) || {}
      : drawRegions(groupKeys, lay);
  performance.mark('graph:backdrop');
  const edgesLayer = s('g', { class: 'edges', 'aria-hidden': 'true' });
  // An opened scope's stubs (under the relations) and far-scope tags (over everything).
  const stubsLayer = s('g', { class: 'xs-stubs', 'aria-hidden': 'true' });
  const tagsLayer = s('g', { class: 'xs-tags' });
  let stubEls = g.nodes.map(() => []);
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
    nodeEls.push(el);
    nodesLayer.appendChild(el);
    // A placed label now; an unplaced one (no free spot: hidden, shown only
    // while its memory is highlighted, CSS .lbl.unplaced) the first time a
    // highlight needs it (`labelEl`), since most of a large graph's labels
    // have no spot and would only cost their creation.
    const lp = lay.labels[i];
    if (lp && lp.placed) {
      const lbl = makeLabel(i);
      labelEls.push(lbl);
      labelsLayer.appendChild(lbl);
    } else labelEls.push(lp ? undefined : null);
  });
  // Each memory's pointer, focus and key handling, delegated to the layer
  // (one listener per event instead of six per memory): the memory is the
  // `.node` the event came from, its index in `data-i`.
  // The view a zoom gesture started from, while one runs (gestureZoom).
  let shown = null;
  let gestureTimer = null;
  const nodeOf = (ev) => {
    const el = ev.target instanceof Element ? ev.target.closest('.node') : null;
    return el && nodesLayer.contains(el) ? el : null;
  };
  // mouseenter / mouseleave: moves within one memory's own parts are not
  // an enter or a leave.
  const crossed = (el, other) => !(other instanceof Node && el.contains(other));
  nodesLayer.addEventListener('mouseover', (ev) => {
    // Mid-zoom the map moves under a still pointer: no hover until it settles.
    const el = shown ? null : nodeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) hover({ node: +el.dataset.i });
  });
  nodesLayer.addEventListener('mouseout', (ev) => {
    const el = nodeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) hover(null);
  });
  // A neighbour focused by ] / [ keeps the selection's highlight (stepRow).
  nodesLayer.addEventListener('focusin', (ev) => {
    const el = nodeOf(ev);
    if (el && ev.target === el && !quietFocus) hover({ node: +el.dataset.i });
  });
  nodesLayer.addEventListener('focusout', (ev) => {
    const el = nodeOf(ev);
    if (el && ev.target === el) hover(null);
  });
  nodesLayer.addEventListener('click', (ev) => {
    const el = nodeOf(ev);
    if (!el) return;
    ev.stopPropagation();
    if (!dragged) pin({ node: +el.dataset.i }, false);
  });
  nodesLayer.addEventListener('keydown', (ev) => {
    const el = nodeOf(ev);
    if (el && (ev.key === 'Enter' || ev.key === ' ')) {
      ev.preventDefault();
      ev.stopPropagation();
      pin({ node: +el.dataset.i }, true);
    }
  });
  performance.mark('graph:nodes');

  const svg = s(
    'svg',
    { class: 'graph', role: 'group', 'aria-label': 'Memories' },
    s('rect', { class: 'bg', x: -1e5, y: -1e5, width: 2e5, height: 2e5, 'aria-hidden': 'true' }),
    backdrop.below || null,
    stubsLayer,
    edgesLayer,
    backdrop.above || null,
    labelsLayer,
    nodesLayer,
    tagsLayer,
  );
  // The <svg>'s own box, which a zoom gesture transforms (gestureZoom): a
  // transform on the <svg> itself re-lays it out and re-paints it.
  const pane = h('div', { class: 'g-pane' }, svg);
  mount(graphBox, pane, tip, hint);
  performance.mark('graph:mounted');

  // ---- an opened scope's far scopes: one tag each, stubs into it ----------
  // Placed once from the unfiltered counts (so a display filter never moves
  // a tag; one with nothing left under the filters is not drawn).
  let viewBounds = lay.bounds;
  const tagByName = new Map();
  if (opened && g.stub_scopes && g.stub_scopes.length) {
    const bw = boxSize.w || 800;
    const bh = boxSize.h || 500;
    const dirs = new Map();
    const ov = overviewLayout(g, bw, Math.min(3, Math.max(0.3, bh / bw)));
    const me = ov.index.get(opened === '-' ? '' : opened);
    for (const sc of g.stub_scopes) {
      const j = ov.index.get(sc.name);
      if (me !== undefined && j !== undefined) dirs.set(sc.name, [ov.lay.pos[2 * j] - ov.lay.pos[2 * me], ov.lay.pos[2 * j + 1] - ov.lay.pos[2 * me + 1]]);
    }
    const tags = g.stub_scopes
      .slice()
      .sort((a, b) => b.count - a.count || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
      .map((sc) => ({ name: sc.name, text: tagText(sc.name, sc.count, sc.contradictions) }));
    const placed = placeTags(lay.bounds, dirs, tags, bw, bh);
    viewBounds = placed.bounds;
    for (const tg of placed.tags) tagByName.set(tg.name, tg);
  }

  /** Stubs from memories to their far scope's tag, and the tags, under the display filters. */
  function drawStubs() {
    if (!tagByName.size) return;
    stubEls = g.nodes.map(() => []);
    const lines = [];
    for (const st of g.stubs || []) {
      const i = byId.get(st.node);
      const tg = tagByName.get(st.scope);
      const ft = filtered(st, f);
      if (i === undefined || !tg || !ft.count) continue;
      const [x, y] = P(i);
      let [x2, y2] = [tg.ax, tg.ay];
      // Zoomed in (cut mode): a stub whose memory is off screen is not
      // drawn, one whose tag is off screen is cut short, like a relation.
      if (cutMode) {
        const sc = (boxSize.w || 1) / view.w;
        const inView = (qx, qy) => qx >= view.x && qx <= view.x + view.w && qy >= view.y && qy <= view.y + view.h;
        if (!inView(x, y)) continue;
        if (!inView(x2, y2)) {
          const dx = x2 - x;
          const dy = y2 - y;
          const len = Math.sqrt(dx * dx + dy * dy) || 1;
          const reach = Math.min(len, 30 / sc);
          x2 = x + (dx / len) * reach;
          y2 = y + (dy / len) * reach;
        }
      }
      const el = s('line', {
        class: ['xs-stub', ft.contradictions && 'contra'],
        x1: x.toFixed(1),
        y1: y.toFixed(1),
        x2: x2.toFixed(1),
        y2: y2.toFixed(1),
        'stroke-width': (0.8 + 0.45 * Math.min(4, ft.count - 1)).toFixed(2),
        'vector-effect': 'non-scaling-stroke',
      });
      stubEls[i].push(el);
      lines.push(el);
    }
    // Many stubs would veil the memories: past STUBS_FAINT they are drawn
    // fainter and come up with their memory's highlight.
    stubsLayer.classList.toggle('many', lines.length > STUBS_FAINT);
    mount(stubsLayer, lines);
    const tagEls = [];
    for (const sc of g.stub_scopes) {
      const tg = tagByName.get(sc.name);
      const ft = filtered(sc, f);
      if (!tg || !ft.count) continue;
      const go = () => (location.hash = hashOf(ctx, { ...f, top_scope: sc.name || '-' }));
      const el = s(
        'g',
        { class: ['xs-tag', ft.contradictions && 'contra'], transform: `translate(${tg.x.toFixed(1)},${tg.y.toFixed(1)})`, tabindex: '0', role: 'link', 'aria-label': t('tagAria', scopeName(sc.name), ft.count, ft.contradictions) },
        s('rect', { x: (-tg.w / 2).toFixed(1), y: (-tg.h / 2).toFixed(1), width: tg.w.toFixed(1), height: tg.h.toFixed(1), rx: (tg.h / 2).toFixed(1) }),
        s('text', { y: (tg.font * 0.36).toFixed(2), 'font-size': tg.font.toFixed(2), 'text-anchor': 'middle' }, `${scopeName(sc.name)} · ${fmt(ft.count)}`, ft.contradictions ? s('tspan', { class: 'w' }, `  ⚠ ${fmt(ft.contradictions)}`) : null),
      );
      el.addEventListener('click', (ev) => {
        ev.stopPropagation();
        if (!dragged) go();
      });
      el.addEventListener('keydown', (ev) => {
        if (ev.key === 'Enter' || ev.key === ' ') {
          ev.preventDefault();
          ev.stopPropagation();
          go();
        }
      });
      tagEls.push(el);
    }
    mount(tagsLayer, tagEls);
  }

  // ---- relations (redrawn when the display filters change) ----------------
  // Built relation elements by index, kept while what they were drawn for
  // (`builtKey`: the scale, and the view in cut mode) holds. A display
  // filter then only hides and shows them (`hidden`) instead of building
  // and styling every drawn relation again: at 487 relations that was half
  // of a toggle. They stay in index order, as a rebuild would put them, so
  // a toggle never changes what overlaps what. A relation is built the
  // first time it is shown (`undefined`: not yet; `null`: nothing to draw).
  let built = [];
  let builtKey = null;
  function buildEdge(k, scale, onScreen) {
    const e = g.edges[k];
    const si = byId.get(e.src);
    const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
    let pts = lay.routes[k];
    let drawn = null;
    if (cutMode && ti >= 0) {
      const a = P(si);
      const b = P(ti);
      const ina = onScreen(a);
      const inb = onScreen(b);
      if (!ina && !inb) return null;
      if (!ina || !inb) {
        // A short piece at the end on screen, still drawn source → target
        // so a style's direction cue (taper, chevron) keeps its meaning.
        const [p0, p1] = ina ? [a, b] : [b, a];
        const dx = p1[0] - p0[0];
        const dy = p1[1] - p0[1];
        const len = Math.sqrt(dx * dx + dy * dy) || 1;
        const reach = Math.min(len, 30 / scale);
        const q = [p0[0] + (dx / len) * reach, p0[1] + (dy / len) * reach];
        pts = ina ? [a, q] : [q, b];
        drawn = ina
          ? theme.drawEdge(e, pts, { rs: radius(g.nodes[si]) + 4, rt: 0, stub: true, scale })
          : theme.drawEdge(e, pts, { rs: 0, rt: radius(g.nodes[ti]) + 4, stub: false, scale });
        drawn = { ...drawn, cls: [...(drawn.cls || []), 'cut'] };
      }
    }
    // `scale`: screen px per layout unit now, for a style that sizes
    // what it draws along a relation by the zoom.
    if (!drawn) drawn = theme.drawEdge(e, pts, { rs: radius(g.nodes[si]) + 4, rt: ti >= 0 ? radius(g.nodes[ti]) + 4 : 9, stub: ti < 0, scale });
    // The wide transparent hit path is the core's, drawn last (on top).
    const parts = [drawn.parts, s('path', { class: 'hit', d: pathD(pts) })];
    return s('g', { class: [...edgeClass(e.kind, e.status, e.state), ...(drawn.cls || [])], 'data-k': k }, parts);
  }
  // Pointer handling for every relation, on the layer (the relation is the
  // `[data-k]` group the event came from), as for the memories.
  const edgeOf = (ev) => {
    const el = ev.target instanceof Element ? ev.target.closest('[data-k]') : null;
    return el && edgesLayer.contains(el) ? el : null;
  };
  edgesLayer.addEventListener('mouseover', (ev) => {
    const el = shown ? null : edgeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) hover({ edge: +el.dataset.k });
  });
  edgesLayer.addEventListener('mouseout', (ev) => {
    const el = edgeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) hover(null);
  });
  edgesLayer.addEventListener('click', (ev) => {
    const el = edgeOf(ev);
    if (!el || dragged) return;
    ev.stopPropagation();
    pin({ edge: +el.dataset.k }, false);
  });
  function drawEdges() {
    const ks = visible();
    const scale = view ? (boxSize.w || 1) / view.w : 1;
    drawnScale = scale;
    // Zoomed to 3× Fit or more on a big graph: a relation with one end off
    // screen is drawn as a short stub from the end on screen, and one with
    // both ends off is not drawn (module docs).
    cutMode = big && !!view && fitView().w / view.w >= CUT_ZOOM;
    const pad = 8 / scale;
    const onScreen = (q) => q[0] >= view.x - pad && q[0] <= view.x + view.w + pad && q[1] >= view.y - pad && q[1] <= view.y + view.h + pad;
    const key = cutMode ? `cut|${scale}|${view.x}|${view.y}|${view.w}|${view.h}` : `${scale}`;
    if (key !== builtKey) {
      builtKey = key;
      built = new Array(g.edges.length);
      edgesLayer.replaceChildren();
    }
    edgeEls = new Array(g.edges.length).fill(null);
    adj = g.nodes.map(() => []);
    const want = new Uint8Array(g.edges.length);
    for (const k of ks) {
      const e = g.edges[k];
      const si = byId.get(e.src);
      const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
      // Adjacency counts every visible relation, drawn whole, cut or not
      // (the hub ring, tooltip and card must not change with the view).
      adj[si].push(k);
      if (ti >= 0) adj[ti].push(k);
      if (built[k] === undefined) built[k] = buildEdge(k, scale, onScreen);
      if (built[k]) {
        edgeEls[k] = built[k];
        want[k] = 1;
      }
    }
    // Index order, back to front: each element goes before the one after it.
    let next = null;
    for (let k = built.length - 1; k >= 0; k--) {
      const el = built[k];
      if (!el) continue;
      if (want[k]) el.removeAttribute('hidden');
      else el.setAttribute('hidden', '');
      if (el.parentNode !== edgesLayer || el.nextSibling !== next) edgesLayer.insertBefore(el, next);
      next = el;
    }
    for (let i = 0; i < g.nodes.length; i++) {
      nodeEls[i].classList.toggle('hub', adj[i].length >= 2);
    }
    return ks;
  }

  // ---- pan / zoom -----------------------------------------------------------
  let view = viewports.get(viewKey) || null;
  let drawnScale = null;
  let settleTimer = null;
  let zoomedOnce = false;
  let cutMode = false;
  function apply() {
    if (shown) {
      clearTimeout(gestureTimer);
      shown = null;
      pane.classList.remove('zooming');
      pane.style.transform = '';
    }
    svg.setAttribute('viewBox', `${view.x.toFixed(1)} ${view.y.toFixed(1)} ${view.w.toFixed(1)} ${view.h.toFixed(1)}`);
    const scale = (boxSize.w || 1) / view.w;
    svg.classList.toggle('z-near', scale >= 0.8);
    svg.classList.toggle('z-mid', scale >= 0.5 && scale < 0.8);
    // A style may follow the zoom with one write on the <svg> (Plain: its
    // relations' width); never per relation. Even that one write restyles
    // every relation that reads it (measured: 58 → 30 fps zooming 534
    // relations), so it is made once the view settles, and once now for the
    // first view; mid-gesture relations scale with the map.
    if (theme.zoomed && !zoomedOnce) {
      zoomedOnce = true;
      theme.zoomed(svg, scale);
    }
    // A style that sizes detail along a relation by the zoom may ask for the
    // relations to be redrawn once a zoom settles far enough from the one
    // they were drawn at (Plain: dashes, chevrons, zigzags). Only then, and
    // only the relations: never per frame.
    // A big graph also redraws when a view settles in (or out of) the
    // zoomed-in cut mode, and on every settle while in it, since what is on
    // screen moved.
    if (theme.zoomed || theme.redrawOnZoom || big) {
      clearTimeout(settleTimer);
      settleTimer = setTimeout(() => {
        if (!svg.isConnected) return;
        const now = (boxSize.w || 1) / view.w;
        if (theme.zoomed) theme.zoomed(svg, now);
        if (drawnScale === null) return;
        const cutNow = big && fitView().w / view.w >= CUT_ZOOM;
        if (cutNow || cutNow !== cutMode || (theme.redrawOnZoom && theme.redrawOnZoom(drawnScale, now))) {
          drawEdges();
          drawStubs();
          highlight(hovered || pinned);
        }
      }, 160);
    }
    viewports.set(viewKey, { ...view });
    while (viewports.size > 8) viewports.delete(viewports.keys().next().value);
  }
  /** The view Fit shows: every memory (and an opened scope's tags). */
  function fitView() {
    const b = viewBounds;
    const aspect = (boxSize.h || 400) / (boxSize.w || 600);
    // Keep the box's aspect so a drag maps 1:1 and nothing is distorted.
    let w = b.w;
    let hh = b.w * aspect;
    if (hh < b.h) {
      hh = b.h;
      w = b.h / aspect;
    }
    return { x: b.x + (b.w - w) / 2, y: b.y + (b.h - hh) / 2, w, h: hh };
  }
  function fit() {
    view = fitView();
    apply();
  }
  /** The view `factor` times as wide, around (cx, cy) (default: its centre). */
  function zoomedView(factor, cx, cy) {
    const px = cx ?? view.x + view.w / 2;
    const py = cy ?? view.y + view.h / 2;
    const w = Math.min(Math.max(view.w * factor, 120), viewBounds.w * 6 + 2000);
    const k = w / view.w;
    return { x: px - (px - view.x) * k, y: py - (py - view.y) * k, w, h: view.h * k };
  }
  function zoom(factor, cx, cy) {
    view = zoomedView(factor, cx, cy);
    apply();
  }
  // A wheel or pinch zoom moves the drawn map as one composited layer (a CSS
  // transform of the <svg> from the view it shows, `shown`) and sets the view
  // box only once the gesture settles: re-painting every memory and relation
  // at each step cost most of a zoom frame (measured in
  // docs/contributors/dashboard-performance.md). Anything else that moves the
  // view goes through apply(), which settles a gesture first.
  function gestureZoom(factor, cx, cy) {
    if (!shown) {
      shown = { ...view };
      pane.classList.add('zooming');
    }
    view = zoomedView(factor, cx, cy);
    const a = shown.w / view.w;
    const bx = ((shown.x - view.x) * (boxSize.w || 1)) / view.w;
    const by = ((shown.y - view.y) * (boxSize.h || 1)) / view.h;
    pane.style.transform = `translate(${bx}px, ${by}px) scale(${a})`;
    clearTimeout(gestureTimer);
    gestureTimer = setTimeout(apply, 160);
  }
  function centerOn(x, y) {
    view = { ...view, x: x - view.w / 2, y: y - view.h / 2 };
    apply();
  }
  const toMap = (clientX, clientY) => {
    // Mid-gesture the <svg>'s box is transformed and shows `shown` whole.
    const v = shown || view;
    const rect = svg.getBoundingClientRect();
    return [v.x + ((clientX - rect.left) / rect.width) * v.w, v.y + ((clientY - rect.top) / rect.height) * v.h];
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
    if (shown && pointers.size < 2) apply();
    const rect = svg.getBoundingClientRect();
    if (pointers.size >= 2 && start.dist) {
      const [a, b] = [...pointers.values()];
      const dist = Math.hypot(a[0] - b[0], a[1] - b[1]) || 1;
      const [cx, cy] = toMap((a[0] + b[0]) / 2, (a[1] + b[1]) / 2);
      // Target width relative to the gesture's start, as a factor of now.
      gestureZoom((start.view.w * start.dist) / dist / view.w, cx, cy);
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
      // Only when shown: setting it again re-lays out the box on every step.
      if (!hint.hidden) hint.hidden = true;
      const [cx, cy] = toMap(ev.clientX, ev.clientY);
      gestureZoom(Math.exp(ev.deltaY * 0.0015), cx, cy);
    },
    { passive: false },
  );
  svg.querySelector('.bg').addEventListener('click', () => {
    if (!dragged) unpin();
  });

  // Keyboard: the graph is one tab stop; ] and [ walk the memories, or the
  // selected memory's panel rows (stepRow).
  const selectedRows = () => !!(pinned && pinned.node !== undefined && panelRows.length);
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
    else if ((ev.key === ']' || ev.key === '[') && selectedRows()) stepRow(ev.key === ']' ? 1 : -1);
    else if (ev.key === ']') stepNode(1);
    else if (ev.key === '[') stepNode(-1);
    else if (ev.key === 'Enter' && !onNode && selectedRows() && rowAt >= 0 && panelRows[rowAt].go) panelRows[rowAt].go();
    else if (ev.key === 'Escape') {
      unpin();
      graphBox.focus();
    } else return;
    ev.preventDefault();
  });
  // The panel takes ] / [ and Escape too, so stepping goes on from a row
  // (a review's own confirm step keeps its Escape: it stops it there).
  card.addEventListener('keydown', (ev) => {
    if ((ev.key === ']' || ev.key === '[') && selectedRows()) {
      ev.preventDefault();
      stepRow(ev.key === ']' ? 1 : -1);
    } else if (ev.key === 'Escape' && pinned) {
      ev.preventDefault();
      unpin();
      graphBox.focus();
    }
  });
  if (view) apply();
  else fit();
  performance.mark('graph:view');
  if (typeof ResizeObserver === 'function') {
    let lastW = boxSize.w;
    const obs = new ResizeObserver(() => {
      if (!graphBox.isConnected) {
        obs.disconnect();
        if (resizeObs === obs) resizeObs = null;
        return;
      }
      // Layout is clean here: reading the box costs nothing.
      boxSize.w = graphBox.clientWidth;
      boxSize.h = graphBox.clientHeight;
      if (Math.abs(boxSize.w - lastW) > 2) {
        lastW = boxSize.w;
        fit();
      }
    });
    obs.observe(graphBox);
    resizeObs = obs;
  }

  // ---- highlighting, the tooltip and the card -----------------------------
  let lit = [];
  // Set while ] / [ moves focus to a neighbour: its focus is not a hover.
  let quietFocus = false;
  let pinned = null; // {edge: k} | {node: i}
  let hovered = null; // the same, while hovered or focused
  function highlight(sel) {
    for (const el of lit) el.classList.remove('on', 'nb', 'nolbl');
    lit = [];
    restoreLabels();
    hiddenNames = [];
    svg.classList.toggle('has-focus', !!sel);
    if (!sel) return;
    const mark = (el) => {
      if (!el) return;
      el.classList.add('on');
      lit.push(el);
    };
    const markNode = (i, nb) => {
      mark(nodeEls[i]);
      mark(labelEl(i));
      if (nb) {
        for (const el of [nodeEls[i], labelEl(i)]) {
          if (!el) continue;
          el.classList.add('nb');
          lit.push(el);
        }
      }
    };
    if (sel.node !== undefined) {
      for (const el of stubEls[sel.node]) mark(el);
      for (const k of adj[sel.node]) {
        mark(edgeEls[k]);
        const e = g.edges[k];
        for (const id of [e.src, e.dst]) if (byId.has(id) && !(id === e.dst && e.dst_store) && byId.get(id) !== sel.node) markNode(byId.get(id), true);
      }
      // Last, so a self-relation never marks the focused memory a neighbour.
      markNode(sel.node, false);
      relabel([sel.node, ...neighboursOf(sel.node)]);
    } else {
      const e = g.edges[sel.edge];
      mark(edgeEls[sel.edge]);
      const ends = [];
      for (const id of [e.src, e.dst]) if (!(id === e.dst && e.dst_store) && byId.has(id)) ends.push(byId.get(id));
      for (const i of ends) markNode(i);
      relabel(ends);
    }
  }

  /** Memory `i`'s neighbours on the graph (indices, once each, not `i`). */
  function neighboursOf(i) {
    const out = new Set();
    for (const k of adj[i]) {
      const e = g.edges[k];
      for (const id of [e.src, e.dst]) if (byId.has(id) && !(id === e.dst && e.dst_store) && byId.get(id) !== i) out.add(byId.get(id));
    }
    return [...out];
  }

  // The labels a focus shows are placed again among themselves, so none
  // overlaps another (the layout placed them against every memory, and many
  // had no spot there). O(degree): only the lit memories take part; the
  // focused one (first) is placed first and always shown. A label that still
  // finds no spot is hidden (`.nolbl`) and named in the tooltip instead (the
  // panel lists every neighbour anyway). Moved labels go back on the next
  // highlight.
  let moved = [];
  let hiddenNames = [];
  function restoreLabels() {
    for (const [el, x, y, anchor] of moved) {
      el.setAttribute('x', x);
      el.setAttribute('y', y);
      el.setAttribute('text-anchor', anchor);
    }
    moved = [];
  }
  function relabel(idx) {
    if (idx.length < 2) return;
    const pos = [];
    const nodes = idx.map((i, at) => {
      const [x, y] = P(i);
      pos.push(x, y);
      const n = g.nodes[i];
      return { id: n.id, r: radius(n), label: labels[i], degree: at === 0 ? 1e9 : n.degree, weight: reads(n) };
    });
    const out = placeLabels(nodes, pos, [], { corners: true });
    out.forEach((lp, at) => {
      const el = labelEl(idx[at]);
      if (!el) return;
      if (!lp.placed && at > 0) {
        el.classList.add('nolbl');
        hiddenNames.push(labels[idx[at]]);
        return;
      }
      moved.push([el, el.getAttribute('x'), el.getAttribute('y'), el.getAttribute('text-anchor')]);
      el.setAttribute('x', lp.x);
      el.setAttribute('y', lp.y);
      el.setAttribute('text-anchor', lp.anchor);
    });
  }

  /**
   * Hover / focus: highlight and a tooltip. Never touches the card.
   *
   * Leaving is applied a frame later and cancelled by the next enter: moving
   * from one memory to the next fires leave then enter in one task, and
   * taking `.has-focus` off the <svg> and putting it back costs Blink an
   * invalidation walk over every memory and relation each time (measured
   * ~8 ms at 800 memories, against ~1 ms for the enter alone).
   */
  let leaveFrame = 0;
  function hover(sel) {
    if (!sel) {
      // Nothing hovered (a leave the map made moving under the pointer).
      if (!hovered && !leaveFrame) return;
      if (!leaveFrame) {
        leaveFrame = requestAnimationFrame(() => {
          leaveFrame = 0;
          showHover(null);
        });
      }
      return;
    }
    if (leaveFrame) {
      cancelAnimationFrame(leaveFrame);
      leaveFrame = 0;
    }
    showHover(sel);
  }
  function showHover(sel) {
    hovered = sel;
    highlight(sel || pinned);
    if (!sel) {
      tip.hidden = true;
      return;
    }
    let x;
    let y;
    // A memory's tooltip sits in the corner of the box away from it, so it
    // never covers the neighbour labels its focus just revealed; none for the
    // selected memory, whose panel says it all.
    let corner = false;
    if (sel.node !== undefined) {
      if (pinned && pinned.node === sel.node) {
        tip.hidden = true;
        return;
      }
      const n = g.nodes[sel.node];
      [x, y] = P(sel.node);
      corner = true;
      const more = hiddenNames.length ? h('span', { class: 'tip-more' }, t('tipMore', hiddenNames.slice(0, 4), hiddenNames.length)) : null;
      mount(tip, h('strong', null, n.summary), h('span', { class: 'muted' }, ` · ${TYPE_LABEL[n.type] || n.type} · ${plural(adj[sel.node].length, 'relation')}`), more);
    } else {
      const e = g.edges[sel.edge];
      const m = along(lay.routes[sel.edge], 0.5);
      x = m.x;
      y = m.y;
      const name = (id) => (byId.has(id) && !(id === e.dst && e.dst_store) ? shortLabel(g.nodes[byId.get(id)].summary, 32) : shortId(id));
      mount(tip, h('strong', null, kindLabel(e.kind)), h('span', { class: 'muted' }, ` (${label(e.status)}) · ${name(e.src)} ${e.symmetric ? '↔' : '→'} ${name(e.dst)}`));
    }
    const [sx, sy] = toScreen(x, y);
    // Measured in the next frame, not here: reading its size now would force
    // the style recalc the focus classes just queued, inside the handler, and
    // then the frame recalculates again (graph.js module docs: hover cost).
    tip.hidden = false;
    tip.classList.add('measuring');
    const at = ++tipSeq;
    requestAnimationFrame(() => {
      if (at !== tipSeq || tip.hidden) return;
      const w = graphBox.clientWidth;
      const bh = graphBox.clientHeight;
      tip.style.maxWidth = Math.min(320, w - 16) + 'px';
      const tw = tip.offsetWidth;
      const th = tip.offsetHeight;
      if (corner) {
        tip.style.left = (sx < w / 2 ? w - tw - 8 : 8) + 'px';
        tip.style.top = (sy < bh / 2 ? bh - th - 8 : 8) + 'px';
      } else {
        tip.style.left = Math.max(8, Math.min(w - tw - 8, sx - tw / 2)) + 'px';
        tip.style.top = Math.max(8, sy - th - 14) + 'px';
      }
      tip.classList.remove('measuring');
    });
  }
  let tipSeq = 0;

  function pin(sel, focusCard) {
    pinned = sel;
    if (reviewNote && !(sel.node !== undefined && g.nodes[sel.node].id === reviewNote.node)) reviewNote = null;
    clearCur();
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
    clearCur();
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
  // status: the reloaded graph pins the edge's source memory (from the
  // panel: the selected memory) and repeats it.
  let reviewNote = null;
  // The selected memory's panel rows, in panel order, and the one ] / [
  // last moved to (-1: none). A live region says which row that is when it
  // is not a memory on the graph (focus then stays on the graph box).
  let panelRows = [];
  let rowAt = -1;
  const announce = h('p', { class: 'sr-only', 'aria-live': 'polite' });
  // The neighbour the current row names, ringed (`.cur`) while focus stays
  // in the panel (a stacked phone layout, where the graph is off screen).
  let curNode = -1;
  function clearCur() {
    if (curNode >= 0 && nodeEls[curNode]) nodeEls[curNode].classList.remove('cur');
    curNode = -1;
  }
  function stepRow(dir) {
    if (rowAt >= 0 && panelRows[rowAt]) panelRows[rowAt].li.classList.remove('cur');
    clearCur();
    rowAt = rowAt < 0 ? (dir > 0 ? 0 : panelRows.length - 1) : (rowAt + dir + panelRows.length) % panelRows.length;
    const row = panelRows[rowAt];
    row.li.classList.add('cur');
    // Stacked (phone): the graph is far above the panel, so focus stays on
    // the row's own button and the neighbour is only ringed. Otherwise
    // focus goes to the neighbour on the graph (quietly: the selection's
    // highlight stays).
    const stacked = window.matchMedia && window.matchMedia('(max-width: 980px)').matches;
    quietFocus = true;
    try {
      if (row.node !== null) {
        const [x, y] = P(row.node);
        if (x < view.x || x > view.x + view.w || y < view.y || y > view.y + view.h) centerOn(x, y);
        nodeEls[row.node].classList.add('cur');
        curNode = row.node;
      }
      const btn = row.li.querySelector('button');
      if (stacked && btn) {
        btn.focus({ preventScroll: true });
        row.li.scrollIntoView({ block: 'nearest' });
      } else {
        row.li.scrollIntoView({ block: 'nearest' });
        if (row.node !== null) nodeEls[row.node].focus({ preventScroll: true });
        else graphBox.focus({ preventScroll: true });
      }
    } finally {
      quietFocus = false;
    }
    // Always said in words ("Supported by: …"): a focused memory's own name
    // carries no direction. Cleared first so the same row twice is said twice.
    announce.textContent = '';
    requestAnimationFrame(() => {
      if (panelRows[rowAt] === row) announce.textContent = row.text;
    });
  }
  // `ids`: the memories to come back to, in order; the first still drawn
  // after the reload is selected (a loser the review superseded is gone
  // unless the Invalidated filter is on, so its other end takes over).
  const afterReview = (ids) => (out, text) => {
    afterReviewPins.set(p.id, { ids, text });
    setTimeout(() => ctx.refresh(), 900);
  };
  const otherEnd = (e, id) => (e.dst_store ? null : e.src === id ? e.dst : e.src);
  /** The review's result line: empty when inserted, its text set a moment
   * later, so a screen reader announces it (a live region that arrives with
   * its text already in it often is not). */
  function noteEl(rn) {
    const el = h('p', { class: 'review-status', role: 'status', 'aria-live': 'polite', tabindex: '-1' });
    if (rn.said) el.textContent = rn.text;
    else
      setTimeout(() => {
        if (!el.isConnected) return;
        el.textContent = rn.text;
        rn.said = true;
      }, 120);
    return el;
  }

  /** The card shows the pinned memory or relation, or the graph summary. */
  function renderCard() {
    const sel = pinned;
    const top = (what) => h('div', { class: 'card-top' }, h('div', { class: 'eyebrow' }, `${what} · pinned`), h('button', { type: 'button', class: 'btn sm ghost', onclick: () => unpin() }, 'Unpin'));
    if (sel && sel.edge !== undefined) {
      panelRows = [];
      rowAt = -1;
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
          ? [h('div', { class: 'eyebrow' }, 'Review'), reviewControls({ ...e, src_summary: src && src.summary, dst_summary: dstNode && dstNode.summary }, ctx, { onDone: afterReview([e.src, otherEnd(e, e.src)].filter(Boolean)) })]
          : null,
      );
      return;
    }
    if (sel && sel.node !== undefined) {
      const n = g.nodes[sel.node];
      const note = reviewNote && reviewNote.node === n.id ? noteEl(reviewNote) : null;
      const panel = memoryPanel({
        g,
        byId,
        i: sel.node,
        ks: adj[sel.node],
        f,
        t,
        theme,
        ctx,
        writes: !!(ctx.state.session && ctx.state.session.allow_writes),
        note,
        onClear: () => {
          unpin();
          graphBox.focus();
        },
        onSelect: (j) => pin({ node: j }, true),
        onOpenScope: (name) => (location.hash = hashOf(ctx, { ...f, top_scope: name || '-' })),
        review: (e) => {
          const src = byId.has(e.src) ? g.nodes[byId.get(e.src)] : null;
          const dst = !e.dst_store && byId.has(e.dst) ? g.nodes[byId.get(e.dst)] : null;
          // After the reload, the selected memory comes back selected.
          return reviewControls({ ...e, src_summary: src && src.summary, dst_summary: dst && dst.summary }, ctx, { viewer: n.id, onDone: afterReview([n.id, otherEnd(e, n.id)].filter(Boolean)) });
        },
      });
      panelRows = panel.rows;
      rowAt = -1;
      mount(card, panel.parts, announce);
      return;
    }
    panelRows = [];
    rowAt = -1;
    const ks = visible();
    const shown = ks.map((k) => g.edges[k]);
    const proposed = shown.filter((e) => e.status === 'proposed');
    mount(
      card,
      h('div', { class: 'eyebrow' }, 'Details'),
      reviewNote && reviewNote.node === null ? noteEl(reviewNote) : null,
      h('h3', null, t('cardTitle')),
      h('p', { class: 'hint' }, t('cardHint')),
      h(
        'dl',
        null,
        h('dt', null, 'Top-level scopes'),
        h('dd', null, fmt(g.scopes.filter((l) => l.name && l.shown > 0).length)),
        h('dt', null, 'Memories'),
        h('dd', null, g.truncated ? `${fmt(g.shown_nodes)} of ${fmt(g.total_nodes)}` : fmt(g.shown_nodes)),
        h('dt', null, 'Relations'),
        h('dd', null, ks.length === g.edges.length ? fmt(ks.length) : `${fmt(ks.length)} of ${fmt(g.edges.length)} (filtered)`),
        h('dt', null, 'Proposed'),
        h('dd', null, `${fmt(proposed.length)}${proposed.some((e) => e.kind === 'contradicts') ? ` (${fmt(proposed.filter((e) => e.kind === 'contradicts').length)} contradictions)` : ''}`),
        tagByName.size ? [h('dt', null, 'To other scopes'), h('dd', null, `${plural(g.stub_scopes.reduce((n, sc) => n + filtered(sc, f).count, 0), 'relation')} (tags around the graph)`)] : null,
        g.counts.hidden ? [h('dt', null, 'Not drawn'), h('dd', null, `${plural(g.counts.hidden, 'relation')} to memories not drawn here (past the memory limit)`)] : null,
      ),
    );
  }

  // ---- legend (the style's) -------------------------------------------------
  // The legend names the scopes drawn, with their drawn memories.
  mount(legendBox, theme.legend({ ...g, scopes: g.scopes.filter((l) => l.shown > 0).map((l) => ({ ...l, count: l.shown })) }, t));
  performance.mark('graph:legend');

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
        h('a', { class: 'btn sm', href: ctx.href('relations', p.id, relationsQuery(f)) }, t('openInRelations')),
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

  // `first`: the initial draw, which sets the bench's `graph:edges` mark.
  function redraw(first) {
    const ks = drawEdges();
    drawStubs();
    if (first === true) performance.mark('graph:edges');
    if (pinned && pinned.edge !== undefined && !edgeEls[pinned.edge]) pinned = null;
    highlight(pinned);
    // The first draw's filters and notes were built before the layout, from
    // the same filters (`ks` is `visible()`), so only a later redraw rebuilds them.
    if (first !== true) {
      renderFilters();
      renderNotes(ks);
    }
    renderTable(ks);
    renderCard();
  }

  ctl.zoom = zoom;
  ctl.fit = fit;
  ctl.pinEdge = (k, focus) => pin({ edge: k }, focus);
  ctl.redraw = () => redraw(false);
  redraw(true);
  performance.mark('graph:built');
  const back = afterReviewPins.get(p.id);
  if (back) {
    afterReviewPins.delete(p.id);
    const id = back.ids.find((x) => byId.has(x));
    if (id === undefined) {
      // Neither end is drawn now: the summary card carries the result.
      reviewNote = { node: null, text: back.text };
      renderCard();
      const el = card.querySelector('.review-status');
      if (el) el.focus();
    } else {
      reviewNote = { node: id, text: id === back.ids[0] ? back.text : `${back.text} ${t('reviewMoved')}` };
      pin({ node: byId.get(id) }, true);
    }
  }
  if (f.memory) {
    const want = f.memory;
    f.memory = '';
    history.replaceState(null, '', hashOf(ctx, f));
    if (!byId.has(want)) notesBox.append(h('div', { class: 'note', role: 'status' }, t('noteMemoryNotDrawn')));
    else if (!back) pin({ node: byId.get(want) }, true);
  }
}
