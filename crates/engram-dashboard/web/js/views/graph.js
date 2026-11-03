// Route map: lines are logical scopes, stations are memories, connectors are
// typed edges.
//
// The server bounds the graph (`/graph?limit=`, "showing N of M"); the layout
// (graph/layout.js) runs in a Web Worker so a large store never freezes the
// page, and is deterministic, so the same store draws the same map. Pan with
// a drag (or the arrow keys on the focused map), zoom with the wheel, a pinch
// or +/-; 0 fits. Hovering a station or a connector fills the card; a click
// on a station opens the memory, a click on a connector pins it in the card,
// where a proposed edge can be reviewed. The connections table below the map
// is the keyboard and screen-reader way to the same edges.

import { h, s, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, plural, shortId, label } from '../format.js';
import { badge, slot, lineName, kindIcon, stationShape, variant, code, TYPE_LABEL } from '../lines.js';
import { layout } from '../graph/layout.js';
import { KINDS, DIRECTED, kindLabel, connectorClass, connectorSample, chevron, terminal, warnSign, stubCap } from '../graph/glyphs.js';
import { projHead, secHead, disruption, errorBox } from './common.js';
import { reviewControls } from './review.js';

// ---------------------------------------------------------------------------
// Layout worker
// ---------------------------------------------------------------------------

let worker = null;
let workerBroken = false;
let layoutSeq = 0;
const waiting = new Map();

function failWorker() {
  workerBroken = true;
  if (worker) worker.terminate();
  worker = null;
  for (const [, w] of waiting) w.fallback();
  waiting.clear();
}

/** Lay out off the main thread; on the main thread (deferred) if no worker starts. */
function runLayout(input) {
  return new Promise((resolve, reject) => {
    const seq = ++layoutSeq;
    const fallback = () =>
      setTimeout(() => {
        try {
          resolve(layout(input));
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
  });
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
    line: query.get('line') || '',
  };
}

function filterQuery(f) {
  const q = new URLSearchParams();
  if (f.kinds.length !== KINDS.length) q.set('kinds', f.kinds.join(','));
  if (!f.proposed) q.set('proposed', '0');
  if (f.invalidated) q.set('invalidated', '1');
  if (f.rejected) q.set('rejected', '1');
  if (f.line) q.set('line', f.line);
  return q.toString();
}

function go(ctx, f) {
  const qs = filterQuery(f);
  location.hash = ctx.href('graph', ctx.project.id) + (qs ? '?' + qs : '');
}

function filterBar(ctx, f, g) {
  const chip = (pressed, content, onclick, title) => h('button', { type: 'button', class: 'chip', 'aria-pressed': String(pressed), onclick, title }, content);
  const kindChips = KINDS.map((k) =>
    chip(f.kinds.includes(k), [connectorSample(k, 'confirmed', 26), kindLabel(k)], () => {
      const on = f.kinds.includes(k);
      // Never filter everything out: the last kind cannot be switched off.
      if (on && f.kinds.length === 1) return;
      go(ctx, { ...f, kinds: on ? f.kinds.filter((x) => x !== k) : KINDS.filter((x) => x === k || f.kinds.includes(x)) });
    }),
  );
  const toggles = [
    chip(f.proposed, 'Proposed', () => go(ctx, { ...f, proposed: !f.proposed }), 'Edges awaiting review'),
    chip(f.rejected, 'Rejected', () => go(ctx, { ...f, rejected: !f.rejected }), 'Edges a reviewer rejected'),
    chip(f.invalidated, 'Invalidated', () => go(ctx, { ...f, invalidated: !f.invalidated }), 'Memories whose validity window closed'),
  ];
  const sel = h(
    'select',
    { 'aria-label': 'Line shown', onchange: (ev) => go(ctx, { ...f, line: ev.target.value }) },
    h('option', { value: '' }, 'All lines'),
    g.all_lines.map((l) => h('option', { value: l.name || '-', selected: (l.name || '-') === f.line }, `${lineName(l.name)} (${fmt(l.count)})`)),
  );
  return h(
    'div',
    { class: 'filt' },
    h('div', { class: 'filt-row', role: 'group', 'aria-label': 'Connector kinds shown' }, h('span', { class: 'eyebrow lab' }, 'Show'), kindChips),
    h('div', { class: 'filt-row', role: 'group', 'aria-label': 'Also show' }, h('span', { class: 'eyebrow lab' }, 'Include'), toggles, h('label', { class: 'sel' }, sel)),
  );
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

function pathD(pts) {
  return 'M' + pts.map((p) => `${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(' L');
}

function along(pts, t) {
  let total = 0;
  for (let i = 1; i < pts.length; i++) total += Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]);
  let want = total * t;
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1];
    const b = pts[i];
    const l = Math.hypot(b[0] - a[0], b[1] - a[1]);
    if (want <= l || i === pts.length - 1) {
      const k = l ? Math.min(1, want / l) : 0;
      return { x: a[0] + (b[0] - a[0]) * k, y: a[1] + (b[1] - a[1]) * k, ang: Math.atan2(b[1] - a[1], b[0] - a[0]) };
    }
    want -= l;
  }
  return { x: pts[0][0], y: pts[0][1], ang: 0 };
}

/** The point `d` along the first (or, with `fromEnd`, back along the last) segment. */
function offset(pts, d, fromEnd) {
  const [a, b] = fromEnd ? [pts[pts.length - 1], pts[pts.length - 2]] : [pts[0], pts[1]];
  const l = Math.hypot(b[0] - a[0], b[1] - a[1]) || 1;
  const k = Math.min(d, l / 2) / l;
  const ang = Math.atan2(a[1] - b[1], a[0] - b[0]);
  return { x: a[0] + (b[0] - a[0]) * k, y: a[1] + (b[1] - a[1]) * k, ang: fromEnd ? ang : ang + Math.PI };
}

const reads = (n) => n.usage.returned + n.usage.injected + n.usage.fetched;
const radius = (n) => 5 + Math.min(6, Math.sqrt(reads(n)) * 0.55);
const trunc = (t, k = 30) => (t.length > k ? t.slice(0, k - 1) + '…' : t);

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

export async function renderGraph(main, ctx) {
  const p = ctx.project;
  if (!p.available) {
    mount(main, projHead('Route map', p.name, [h('span', { class: 'mono' }, p.path)]), h('div', { class: 'note warn' }, `Unavailable: ${p.unavailable_reason || 'no readable store'}.`));
    return;
  }
  const f = filtersOf(ctx.route.query);
  mount(main, h('p', { class: 'loading' }, 'Loading route map…'));
  const params = {
    include_proposed: f.proposed ? 'true' : 'false',
    include_invalidated: f.invalidated ? 'true' : 'false',
    include_rejected: f.rejected ? 'true' : 'false',
  };
  if (f.kinds.length !== KINDS.length) params.kinds = f.kinds.join(',');
  let g;
  try {
    g = await api.graph(p.id, { ...params, line: f.line || undefined });
  } catch (e) {
    if (ctx.isStale()) return;
    mount(main, projHead('Route map', `${p.name} network`, []), errorBox(e));
    return;
  }
  if (ctx.isStale()) return;

  // Controls rendered before the layout is ready call through here; they do
  // nothing until the map exists.
  const ctl = { zoom() {}, fit() {}, pinEdge() {} };
  const byId = new Map(g.nodes.map((n, i) => [n.id, i]));
  const card = h('aside', { class: 'g-card', 'aria-live': 'polite', 'aria-label': 'Station card' });
  const mapbox = h('div', {
    class: 'mapbox',
    tabindex: '0',
    role: 'group',
    'aria-label': `Route map of ${p.name}: ${plural(g.shown_nodes, 'station')}, ${plural(g.counts.edges, 'connector')}. Arrow keys pan, plus and minus zoom, 0 fits the map.`,
  });
  const legendBox = h('div', { class: 'legend' });
  const tableBox = h('div');

  const meta = ['Lines are logical scopes. Stations are memories.', 'Station size follows reads; shape follows kind.'];
  const disruptions = g.edges.filter((e) => e.kind === 'contradicts' && e.status === 'proposed');
  const notes = [];
  if (g.truncated) {
    notes.push(
      h('div', { class: 'note', role: 'status' }, `Showing ${fmt(g.shown_nodes)} of ${fmt(g.total_nodes)} stations: the most connected first, then the most read. Pick a line to see the rest.`),
    );
  }
  if (g.counts.dangling || g.counts.unknown) {
    const bits = [];
    if (g.counts.dangling) bits.push(`${plural(g.counts.dangling, 'connector')} to a deleted memory (drawn to a closed station ⊗)`);
    if (g.counts.unknown) bits.push(`${plural(g.counts.unknown, 'connector')} into a store that cannot be checked here (drawn to “?”)`);
    notes.push(h('div', { class: 'note' }, bits.join('; ') + '.'));
  }

  mount(
    main,
    projHead('Route map', `${p.name} network`, meta),
    disruptions.length
      ? disruption(
          `${plural(disruptions.length, 'proposed contradiction')} awaiting review.`,
          null,
          h('button', { type: 'button', class: 'btn sm solid', onclick: () => ctl.pinEdge(g.edges.indexOf(disruptions[0]), true) }, 'Review first'),
        )
      : null,
    filterBar(ctx, f, g),
    notes,
    h(
      'div',
      { class: 'g-grid' },
      h(
        'div',
        { class: 'g-main' },
        h(
          'div',
          { class: 'map-tools' },
          h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Zoom in', onclick: () => ctl.zoom(1 / 1.4) }, '+'),
          h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Zoom out', onclick: () => ctl.zoom(1.4) }, '−'),
          h('button', { type: 'button', class: 'btn sm', onclick: () => ctl.fit() }, 'Fit'),
          h('span', { class: 'faint' }, 'Drag to pan · wheel or pinch to zoom'),
        ),
        mapbox,
        legendBox,
      ),
      card,
    ),
    tableBox,
  );

  if (!g.nodes.length) {
    mount(mapbox, h('p', { class: 'map-empty muted' }, f.line ? 'No stations on this line with these filters.' : 'No memories to draw yet.'));
    renderCard(null);
    return;
  }
  mount(mapbox, h('p', { class: 'map-empty muted' }, `Laying out ${plural(g.nodes.length, 'station')}…`));

  // Layout input: edge ends as station indices (-1: a stub off the source).
  const lay = await runLayout({
    nodes: g.nodes.map((n) => ({ id: n.id, line: n.line || '', slot: n.line_slot ?? null, scope: n.scope || '', degree: n.degree })),
    edges: g.edges.map((e) => ({ s: byId.get(e.src), t: e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst) })),
  }).catch((e) => {
    if (!ctx.isStale()) mount(mapbox, errorBox(e));
    return null;
  });
  if (!lay || ctx.isStale()) return;

  // ---- draw ---------------------------------------------------------------
  const P = (i) => [lay.pos[2 * i], lay.pos[2 * i + 1]];
  const nodeEls = [];
  const edgeEls = [];
  const adj = g.nodes.map(() => []);
  g.edges.forEach((e, k) => {
    adj[byId.get(e.src)].push(k);
    if (!e.dst_store && byId.has(e.dst)) adj[byId.get(e.dst)].push(k);
  });

  const linesLayer = s('g', { class: 'lines' });
  for (const l of lay.lines) {
    if (!l.key || l.pts.length < 2) continue;
    linesLayer.appendChild(s('path', { class: ['ln', 'stroke-' + slot(l.slot), 'lv-' + variant(l.slot)], d: pathD(l.pts) }));
  }
  const badges = s('g', { class: 'lbadges' });
  for (const l of lay.lines) {
    if (!l.key) continue;
    const [x, y] = l.pts[0];
    badges.appendChild(
      s('g', { class: 'lb', transform: `translate(${(x - 26).toFixed(1)},${(y - 22).toFixed(1)})` }, s('rect', { class: 'fill-' + slot(l.slot), x: -12, y: -12, width: 24, height: 24, rx: 5 }), s('text', { class: 'lbt-' + slot(l.slot) }, code(l.key))),
    );
  }

  const edgesLayer = s('g', { class: 'edges' });
  g.edges.forEach((e, k) => {
    const pts = lay.routes[k];
    const si = byId.get(e.src);
    const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
    const d = pathD(pts);
    const parts = [s('path', { class: 'halo', d }), s('path', { class: 'cn', d })];
    const rs = radius(g.nodes[si]) + 4;
    const rt = ti >= 0 ? radius(g.nodes[ti]) + 4 : 9;
    if (e.kind === 'contradicts') {
      const a = offset(pts, rs);
      parts.push(terminal(a.x, a.y, a.ang));
      if (ti >= 0) {
        const b = offset(pts, rt, true);
        parts.push(terminal(b.x, b.y, b.ang));
      }
      if (e.status === 'proposed') {
        const m = along(pts, 0.5);
        parts.push(warnSign(m.x, m.y, 0.85));
      }
    } else if (DIRECTED[e.kind]) {
      const b = offset(pts, rt + 2, true);
      parts.push(chevron(b.x, b.y, b.ang + Math.PI));
    }
    if (ti < 0) {
      const end = pts[pts.length - 1];
      parts.push(stubCap(end[0], end[1], e.state === 'unknown' ? 'unknown' : 'dangling'));
    }
    parts.push(s('path', { class: 'hit', d }));
    const el = s('g', { class: connectorClass(e.kind, e.status, e.state), 'data-k': k }, parts);
    el.addEventListener('mouseenter', () => preview({ edge: k }));
    el.addEventListener('mouseleave', () => preview(null));
    el.addEventListener('click', (ev) => {
      if (dragged) return;
      ev.stopPropagation();
      pinEdge(k, false);
    });
    edgeEls.push(el);
    edgesLayer.appendChild(el);
  });

  const stationsLayer = s('g', { class: 'stations' });
  const labelsLayer = s('g', { class: 'labels' });
  g.nodes.forEach((n, i) => {
    const [x, y] = P(i);
    const r = radius(n);
    const el = s(
      'g',
      {
        class: ['st', 'stroke-' + slot(n.line_slot), n.degree >= 2 && 'ix', !reads(n) && 'never', n.challenged && 'chal', n.invalidated && 'inv'],
        transform: `translate(${x},${y})`,
        'data-i': i,
      },
      s('circle', { class: 'ring', r: r + 6 }),
      n.challenged ? s('circle', { class: 'chal-ring', r: r + 3.5 }) : null,
      stationShape(n.type, r),
    );
    el.addEventListener('mouseenter', () => preview({ node: i }));
    el.addEventListener('mouseleave', () => preview(null));
    el.addEventListener('click', () => {
      if (!dragged) location.hash = ctx.href('detail', p.id, n.id);
    });
    nodeEls.push(el);
    stationsLayer.appendChild(el);
    const major = n.degree >= 2 || n.proposed > 0 || n.challenged;
    labelsLayer.appendChild(s('text', { class: ['lbl', major && 'major'], x: x + r + 6, y: y + 4, 'data-i': i }, trunc(n.summary)));
  });

  const svg = s('svg', { class: 'map', role: 'presentation', 'aria-hidden': 'true' }, s('rect', { class: 'bg', x: -1e5, y: -1e5, width: 2e5, height: 2e5 }), linesLayer, edgesLayer, badges, stationsLayer, labelsLayer);
  mount(mapbox, svg);

  // ---- pan / zoom -----------------------------------------------------------
  let view = { ...lay.bounds };
  function apply() {
    svg.setAttribute('viewBox', `${view.x.toFixed(1)} ${view.y.toFixed(1)} ${view.w.toFixed(1)} ${view.h.toFixed(1)}`);
    const scale = mapbox.clientWidth / view.w;
    svg.classList.toggle('z-near', scale >= 1.05);
    svg.classList.toggle('z-mid', scale >= 0.55 && scale < 1.05);
  }
  function fit() {
    const b = lay.bounds;
    const aspect = (mapbox.clientHeight || 400) / (mapbox.clientWidth || 600);
    // Keep the box's aspect so a drag maps 1:1 and nothing is letterboxed.
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
  let dragged = false;
  const pointers = new Map();
  let start = null;
  svg.addEventListener('pointerdown', (ev) => {
    pointers.set(ev.pointerId, [ev.clientX, ev.clientY]);
    dragged = false;
    start = { x: ev.clientX, y: ev.clientY, view: { ...view }, dist: null };
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      start.dist = Math.hypot(a[0] - b[0], a[1] - b[1]);
    }
  });
  svg.addEventListener('pointermove', (ev) => {
    if (!pointers.has(ev.pointerId) || !start) return;
    pointers.set(ev.pointerId, [ev.clientX, ev.clientY]);
    const rect = svg.getBoundingClientRect();
    if (pointers.size >= 2 && start.dist) {
      const [a, b] = [...pointers.values()];
      const dist = Math.hypot(a[0] - b[0], a[1] - b[1]);
      const [cx, cy] = toMap((a[0] + b[0]) / 2, (a[1] + b[1]) / 2);
      zoom(start.dist / dist / (start.view.w / view.w), cx, cy);
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
    if (!pointers.size) start = null;
    // `dragged` stays set through the click that follows the pointerup.
    setTimeout(() => {
      if (!pointers.size) dragged = false;
    }, 0);
  };
  svg.addEventListener('pointerup', end);
  svg.addEventListener('pointercancel', end);
  svg.addEventListener(
    'wheel',
    (ev) => {
      ev.preventDefault();
      const [cx, cy] = toMap(ev.clientX, ev.clientY);
      zoom(Math.exp(ev.deltaY * 0.0015), cx, cy);
    },
    { passive: false },
  );
  svg.querySelector('.bg').addEventListener('click', () => {
    if (!dragged) unpin();
  });
  mapbox.addEventListener('keydown', (ev) => {
    if (ev.target !== mapbox) return;
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
    else if (ev.key === 'Escape') unpin();
    else return;
    ev.preventDefault();
  });
  fit();
  if (typeof ResizeObserver === 'function') {
    let lastW = mapbox.clientWidth;
    new ResizeObserver(() => {
      if (mapbox.isConnected && Math.abs(mapbox.clientWidth - lastW) > 2) {
        lastW = mapbox.clientWidth;
        fit();
      }
    }).observe(mapbox);
  }

  // ---- focus highlighting and the card -----------------------------------
  let lit = [];
  let pinned = null; // {edge: k} | {node: i}
  function highlight(sel) {
    for (const el of lit) el.classList.remove('on');
    lit = [];
    svg.classList.toggle('has-focus', !!sel);
    if (!sel) return;
    const mark = (el) => {
      el.classList.add('on');
      lit.push(el);
    };
    if (sel.node !== undefined) {
      mark(nodeEls[sel.node]);
      for (const k of adj[sel.node]) {
        mark(edgeEls[k]);
        const e = g.edges[k];
        for (const id of [e.src, e.dst]) if (byId.has(id)) mark(nodeEls[byId.get(id)]);
      }
    } else {
      const e = g.edges[sel.edge];
      mark(edgeEls[sel.edge]);
      for (const id of [e.src, e.dst]) if (!(id === e.dst && e.dst_store) && byId.has(id)) mark(nodeEls[byId.get(id)]);
    }
  }
  function preview(sel) {
    const shown = sel || pinned;
    highlight(shown);
    renderCard(shown);
  }
  function pinEdge(k, focusCard) {
    if (k < 0) return;
    pinned = { edge: k };
    const pts = lay.routes[k];
    const m = along(pts, 0.5);
    const b = mapbox.getBoundingClientRect();
    const visible = m.x > view.x && m.x < view.x + view.w && m.y > view.y && m.y < view.y + view.h;
    if (!visible || focusCard) centerOn(m.x, m.y);
    preview(null);
    if (focusCard) {
      const head = card.querySelector('h3');
      if (head) {
        head.setAttribute('tabindex', '-1');
        head.focus({ preventScroll: b.top >= 0 });
      }
    }
  }
  function unpin() {
    pinned = null;
    preview(null);
  }

  function otherEnd(e, id) {
    const oid = e.src === id ? e.dst : e.src;
    const i = byId.get(oid);
    return { id: oid, node: i === undefined || (e.dst_store && oid === e.dst) ? null : g.nodes[i] };
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

  function renderCard(sel) {
    if (sel && sel.edge !== undefined) {
      const e = g.edges[sel.edge];
      const src = g.nodes[byId.get(e.src)];
      const dstNode = !e.dst_store && byId.has(e.dst) ? g.nodes[byId.get(e.dst)] : null;
      const isPinned = pinned && pinned.edge === sel.edge;
      mount(
        card,
        h('div', { class: 'card-top' }, h('div', { class: 'eyebrow' }, isPinned ? 'Connector · pinned' : 'Connector'), isPinned ? h('button', { type: 'button', class: 'btn sm ghost', onclick: unpin }, 'Unpin') : null),
        h('h3', null, connectorSample(e.kind, e.status, 40), ' ', kindLabel(e.kind)),
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
        e.state === 'dangling' ? h('p', { class: 'hint' }, 'The far end was deleted. The edge stays in the source file until someone unlinks it.') : null,
        e.state === 'unknown' ? h('p', { class: 'hint' }, 'The far end is in another store that this dashboard could not open, so it is unknown, not missing.') : null,
        e.status === 'proposed'
          ? [
              h('div', { class: 'eyebrow' }, 'Review'),
              reviewControls({ ...e, src_summary: src && src.summary, dst_summary: dstNode && dstNode.summary }, ctx, { onDone: () => setTimeout(() => ctx.refresh(), 600) }),
            ]
          : null,
      );
      return;
    }
    if (sel && sel.node !== undefined) {
      const n = g.nodes[sel.node];
      const items = adj[sel.node].slice(0, 12).map((k) => {
        const e = g.edges[k];
        const o = otherEnd(e, n.id);
        const dir = e.symmetric ? '' : e.src === n.id ? '→ ' : '← ';
        return h(
          'li',
          null,
          connectorSample(e.kind, e.status, 30),
          h('span', { class: 'muted' }, ` ${dir}${kindLabel(e.kind).toLowerCase()}${e.status === 'proposed' ? ' (proposed)' : e.status === 'rejected' ? ' (rejected)' : ''}`),
          h('br'),
          o.node ? h('span', { class: 'cn-other' }, o.node.summary) : h('span', { class: 'faint' }, e.state === 'unknown' ? 'other store (unknown)' : 'deleted memory'),
        );
      });
      mount(
        card,
        h('div', { class: 'sign sm' }, badge(n.line, n.line_slot), h('span', { class: 'sc' }, n.scope || 'No logical scope')),
        h('div', { class: 'mono faint small' }, n.id),
        h('h3', null, n.summary),
        h(
          'dl',
          null,
          h('dt', null, 'Kind'),
          h('dd', null, h('span', { class: 'kind' }, kindIcon(n.type, n.line_slot), TYPE_LABEL[n.type] || n.type)),
          h('dt', null, 'Status'),
          h('dd', null, n.invalidated ? 'invalidated' : label(n.status)),
          h('dt', null, 'Reads'),
          h('dd', null, `${fmt(n.usage.returned)} retrieved · ${fmt(n.usage.injected)} injected · ${fmt(n.usage.fetched)} fetched`),
        ),
        h('div', { class: 'eyebrow' }, `Connections (${fmt(n.degree)})`),
        items.length ? h('ul', null, items) : h('p', { class: 'hint' }, 'No connections shown. Check the filters.'),
        h('a', { class: 'btn solid', href: ctx.href('detail', p.id, n.id) }, 'Open memory →'),
      );
      return;
    }
    mount(
      card,
      h('div', { class: 'eyebrow' }, 'Station card'),
      h('h3', null, 'Hover a station'),
      h('p', { class: 'hint' }, 'Hovering lights up a station’s connections. Click a station to open it; click a connector to pin it here and review it.'),
      h(
        'dl',
        null,
        h('dt', null, 'Lines'),
        h('dd', null, fmt(g.lines.filter((l) => l.name).length)),
        h('dt', null, 'Stations'),
        h('dd', null, g.truncated ? `${fmt(g.shown_nodes)} of ${fmt(g.total_nodes)}` : fmt(g.shown_nodes)),
        h('dt', null, 'Connectors'),
        h('dd', null, fmt(g.counts.edges)),
        h('dt', null, 'Proposed'),
        h('dd', null, `${fmt(g.counts.proposed)}${g.counts.proposed_contradictions ? ` (${fmt(g.counts.proposed_contradictions)} contradictions)` : ''}`),
        g.counts.hidden ? [h('dt', null, 'Not drawn'), h('dd', null, `${plural(g.counts.hidden, 'connector')} to stations outside this view`)] : null,
      ),
    );
  }
  renderCard(null);
  ctl.zoom = zoom;
  ctl.fit = fit;
  ctl.pinEdge = pinEdge;

  // ---- legend ---------------------------------------------------------------
  const types = [...new Set(g.nodes.map((n) => n.type))].sort();
  mount(
    legendBox,
    h(
      'div',
      null,
      h('h4', null, 'Lines (logical scope)'),
      h(
        'ul',
        null,
        g.lines.slice(0, 12).map((l) =>
          h('li', null, badge(l.name, l.slot), s('svg', { width: 34, height: 10, 'aria-hidden': 'true' }, s('path', { class: ['ln', 'stroke-' + slot(l.slot), 'lv-' + variant(l.slot)], d: 'M3,5 H31' })), `${lineName(l.name)} · ${fmt(l.count)}`),
        ),
        g.lines.length > 12 ? h('li', { class: 'faint' }, `and ${plural(g.lines.length - 12, 'more line')}`) : null,
      ),
    ),
    h(
      'div',
      null,
      h('h4', null, 'Stations (memories)'),
      h(
        'ul',
        null,
        types.map((t) => h('li', null, kindIcon(t, 0), TYPE_LABEL[t] || t)),
        h('li', null, legendStation('ix'), 'Interchange: 2+ connections'),
        h('li', null, legendStation('never'), 'Never read'),
        h('li', null, legendStation('chal'), 'Challenged'),
        h('li', null, s('svg', { width: 34, height: 16, viewBox: '0 0 34 16', 'aria-hidden': 'true', class: 'lg-size' }, s('circle', { cx: 6, cy: 8, r: 3.5 }), s('circle', { cx: 23, cy: 8, r: 7 })), 'Size follows reads'),
      ),
    ),
    h(
      'div',
      null,
      h('h4', null, 'Connectors (edges)'),
      h(
        'ul',
        null,
        KINDS.map((k) => h('li', null, connectorSample(k, 'confirmed'), kindLabel(k))),
        h('li', null, connectorSample('contradicts', 'proposed'), 'Proposed contradiction (dashed, ⚠)'),
        h('li', null, connectorSample('related_to', 'proposed'), 'Proposed, not yet reviewed'),
        h('li', null, connectorSample('related_to', 'rejected'), 'Rejected'),
        h('li', null, s('svg', { width: 44, height: 16, viewBox: '0 0 44 16', 'aria-hidden': 'true', class: 'cx s-confirmed' }, s('path', { class: 'cn', d: 'M4,8 H30' }), stubCap(36, 8, 'dangling')), 'To a deleted memory'),
        h('li', null, s('svg', { width: 44, height: 16, viewBox: '0 0 44 16', 'aria-hidden': 'true', class: 'cx s-confirmed' }, s('path', { class: 'cn', d: 'M4,8 H30' }), stubCap(36, 8, 'unknown')), 'Into a store not checked here'),
      ),
    ),
  );

  // ---- the accessible fallback: every connector as a table row -------------
  const MAX_ROWS = 400;
  const rows = g.edges.map((e, k) => ({ e, k })).sort((a, b) => (a.e.status === 'proposed' ? 0 : 1) - (b.e.status === 'proposed' ? 0 : 1) || (a.e.kind === 'contradicts' ? 0 : 1) - (b.e.kind === 'contradicts' ? 0 : 1));
  mount(
    tableBox,
    secHead('Connections', h('span', { class: 'faint num' }, rows.length > MAX_ROWS ? `first ${fmt(MAX_ROWS)} of ${fmt(rows.length)}` : plural(rows.length, 'connector'))),
    rows.length
      ? h(
          'div',
          { class: 'tbox' },
          h(
            'table',
            { class: 'edges-table' },
            h('caption', { class: 'sr-only' }, `Connectors on the route map of ${p.name}, proposed ones first.`),
            h(
              'thead',
              null,
              h('tr', null, ['From', 'Relation', 'To', 'Status', 'Origin', 'Conf.', ''].map((t, i) => h('th', { scope: 'col', class: i === 5 ? 'n' : null }, t))),
            ),
            h(
              'tbody',
              null,
              rows.slice(0, MAX_ROWS).map(({ e, k }) =>
                h(
                  'tr',
                  { class: 'plain-row' },
                  h('td', { class: 't' }, edgeEnd(e, 'src')),
                  h('td', null, h('span', { class: 'kind' }, connectorSample(e.kind, e.status, 30), kindLabel(e.kind))),
                  h('td', { class: 't' }, edgeEnd(e, 'dst')),
                  h('td', null, h('span', { class: ['ep', 'st-' + e.status] }, label(e.status)), e.state !== 'live' ? h('span', { class: 'faint' }, ` · ${e.state}`) : null),
                  h('td', null, e.origin),
                  h('td', { class: 'n' }, Number(e.confidence).toFixed(2)),
                  h('td', null, h('button', { type: 'button', class: 'btn sm', onclick: () => pinEdge(k, true) }, e.status === 'proposed' ? 'Review' : 'Show on map')),
                ),
              ),
            ),
          ),
        )
      : h('p', { class: 'muted' }, 'No connectors with these filters. Agents add edges with the link tool; NLI and embedding similarity propose them as memories are created.'),
  );
}

function legendStation(kind) {
  const cls = ['st', 'stroke-0', kind];
  return s('svg', { width: 18, height: 18, viewBox: '-9 -9 18 18', 'aria-hidden': 'true', class: 'lg-st' }, s('g', { class: cls }, kind === 'chal' ? s('circle', { class: 'chal-ring', r: 7.5 }) : null, s('circle', { class: 'sh', r: 5 })));
}
