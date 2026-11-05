// Parity and size check of the server's layouts (src/layout/) against the
// page's JS layouts, on a running dashboard:
//
//   node bench/server-layout.mjs --url 'http://127.0.0.1:7341/#token=…' \
//     [--projects id,id] [--aspect 0.6] [--width 1280] [--scope storage] [--json out.json]
//
// For every available project it asks /graph for what the page asks (the
// overview's summary, the largest scope opened, "show all") with a server
// layout, builds the worker's input from the same response the way
// views/graph.js does, runs the JS layout in this process and compares:
// the largest position / route / label / bounds difference (0 means the
// same bits), labels placed and relation crossings for each, JS time (cold
// = first call in this process, warm = median of 5), the response time
// with and without `layout=` (start the dashboard with
// ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0 for a cold server figure), and the
// encoded layout's size against alternative encodings, raw and gzipped.
// Exits 1 when a layout differs by more than 1e-9 or fails to decode.

import { gzipSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { LAYOUTS } from '../web/js/graph/layouts.js';
import { fromServer } from '../web/js/graph/server-layout.js';
import { reads, radius } from '../web/js/graph/radius.js';
import { subKey } from '../web/js/graph/opened.js';
import { shortLabel } from '../web/js/views/graph.js';
import { scopePairs } from '../web/js/views/graph-overview.js';
import { crossings } from './metrics.mjs';

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith('--') ? [...acc, [a.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : '1']] : acc), []),
);
if (!args.url) {
  console.error('usage: node bench/server-layout.mjs --url <dashboard token URL> [options]');
  process.exit(2);
}
const origin = new URL(args.url).origin;
const token = args.url.split('#token=')[1];
const aspect = Number(args.aspect || 0.6);
const width = Number(args.width || 1280);
const scope = args.scope || 'storage';

async function get(path) {
  const t0 = performance.now();
  const res = await fetch(origin + path, { headers: { Authorization: `Bearer ${token}` } });
  const text = await res.text();
  const ms = performance.now() - t0;
  if (!res.ok) throw new Error(`${path}: ${res.status} ${text}`);
  return { json: JSON.parse(text), ms, bytes: text.length };
}

/** views/graph.js's layout input for a /graph response. */
function forceInput(g, opened) {
  const byId = new Map(g.nodes.map((n, i) => [n.id, i]));
  const groupKeys = g.nodes.map((n) => (opened ? subKey(n) : n.top_scope || ''));
  return {
    layout: 'scope-force',
    nodes: g.nodes.map((n, i) => ({ id: n.id, top_scope: groupKeys[i], slot: n.color_slot ?? null, scope: n.scope || '', degree: n.degree, r: radius(n), label: shortLabel(n.summary), weight: reads(n) })),
    options: {},
    edges: g.edges.map((e) => {
      const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
      return { s: byId.get(e.src), t: ti, kind: e.kind, far_scope: ti >= 0 ? g.nodes[ti].top_scope || '' : (e.far_scope ?? null) };
    }),
    aspect,
    width,
  };
}

/** views/graph-overview.js's overview input for a summary. */
function overviewInput(g) {
  const fmt = (n) => Number(n || 0).toLocaleString('en-US');
  const name = (s) => s || 'No scope';
  const scopes = g.scopes.filter((l) => l.count > 0);
  const index = new Map(scopes.map((l, i) => [l.name, i]));
  const pairs = scopePairs(g).filter((p) => index.has(p.a) && index.has(p.b));
  return {
    layout: 'scope-overview',
    nodes: scopes.map((l) => ({ id: l.name, count: l.count, label: `${name(l.name)} · ${fmt(l.count)}`, name: name(l.name) })),
    edges: pairs.map((p) => ({ s: index.get(p.a), t: index.get(p.b), n: p.tally.count })),
    aspect,
    width,
  };
}

/** The largest absolute difference between two layout outputs. */
function maxDelta(a, b) {
  let d = 0;
  const num = (x, y) => {
    if (typeof x === 'number' && typeof y === 'number') {
      const v = x === y ? 0 : Math.abs(x - y);
      if (!(v <= d)) d = Number.isNaN(v) ? Infinity : v;
    } else if (Array.isArray(x) && Array.isArray(y)) {
      if (x.length !== y.length) d = Infinity;
      else for (let i = 0; i < x.length; i++) num(x[i], y[i]);
    } else if (x && y && typeof x === 'object') {
      for (const k of Object.keys(x)) if (k !== 'box') num(x[k], y[k]);
    } else if (x !== y) d = Infinity;
  };
  num(a, b);
  return d;
}

const segs = (lay, input) => lay.routes.map((rt, k) => [rt[0], rt[rt.length - 1], input.edges[k].s, input.edges[k].t]).filter((s) => s[3] >= 0);

/** Sizes of the positions in other encodings (integers at 1/10 and 1/100 unit). */
function sizes(L, lay) {
  const gz = (s) => gzipSync(Buffer.from(s)).length;
  const json = JSON.stringify(L);
  const out = { json: json.length, json_gz: gz(json) };
  if (L.name === 'scope-force') {
    const i32 = (scale) => {
      const b = Buffer.alloc(lay.pos.length * 4);
      lay.pos.forEach((v, i) => b.writeInt32LE(Math.round(v * scale), i * 4));
      return b.toString('base64');
    };
    const f64 = L.pos;
    const ints10 = JSON.stringify(lay.pos.map((v) => Math.round(v * 10)));
    out.pos_f64_b64 = f64.length;
    out.pos_f64_b64_gz = gz(f64);
    out.pos_i32_100_b64 = i32(100).length;
    out.pos_i32_100_b64_gz = gz(i32(100));
    out.pos_json_int10 = ints10.length;
    out.pos_json_int10_gz = gz(ints10);
    const full = JSON.stringify({ pos: lay.pos, routes: lay.routes, labels: lay.labels, bounds: lay.bounds, scopes: lay.scopes });
    out.worker_output_json = full.length;
    out.worker_output_json_gz = gz(full);
  }
  return out;
}

function check(label, L, input) {
  const name = input.layout;
  const t0 = performance.now();
  const js = LAYOUTS[name](input);
  const cold = performance.now() - t0;
  const warm = [];
  for (let i = 0; i < 5; i++) {
    const t = performance.now();
    LAYOUTS[name](input);
    warm.push(performance.now() - t);
  }
  warm.sort((a, b) => a - b);
  const srv = fromServer(L, input);
  const row = { case: label, layout: name, nodes: input.nodes.length, edges: input.edges.length, js_cold_ms: +cold.toFixed(1), js_warm_ms: +warm[2].toFixed(1) };
  if (!srv) return { ...row, ok: false, problem: L ? 'did not decode / header mismatch' : 'no layout in the response' };
  row.max_delta = maxDelta(js, srv);
  row.labels_placed_js = js.labels.filter((l) => l.placed).length;
  row.labels_placed_server = srv.labels.filter((l) => l.placed).length;
  if (name === 'scope-force') {
    row.crossings_js = crossings(segs(js, input));
    row.crossings_server = crossings(segs(srv, input));
  }
  row.sizes = sizes(L, srv);
  row.ok = row.max_delta <= 1e-9;
  return row;
}

const q = (o) => Object.entries(o).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');
const lay = (names, extra = {}) => ({ layout: names, layout_aspect: aspect, layout_width: width, ...extra });
const base = { include_proposed: 'true', include_rejected: 'true', include_invalidated: 'false' };

const rows = [];
const projects = (await get('/api/projects')).json.projects.filter((p) => p.available && (!args.projects || args.projects.split(',').includes(p.id)));
for (const p of projects) {
  const path = `/api/projects/${p.id}/graph?`;
  // Tab click: the overview's summary (or the whole graph at ≤ 150).
  const ov = await get(path + q({ ...base, summary_over: 150, ...lay('scope-overview,scope-force') }));
  const ovPlain = await get(path + q({ ...base, summary_over: 150 }));
  const tab = ov.json.summary ? check('tab (overview)', ov.json.layout, overviewInput(ov.json)) : check('tab (whole graph)', ov.json.layout, forceInput(ov.json, false));
  rows.push({ project: p.name, ...tab, response_ms: +ov.ms.toFixed(1), response_without_layout_ms: +ovPlain.ms.toFixed(1), response_bytes: ov.bytes });
  if (!ov.json.summary) continue;
  // The largest scope opened, and show all.
  for (const [label, params, opened] of [
    [`open ${scope}`, { ...base, top_scope: scope, ...lay('scope-force', { layout_groups: 'sub' }) }, true],
    ['show all', { ...base, ...lay('scope-force') }, false],
  ]) {
    const r = await get(path + q(params));
    const plain = await get(path + q(Object.fromEntries(Object.entries(params).filter(([k]) => !k.startsWith('layout')))));
    rows.push({ project: p.name, ...check(label, r.json.layout, forceInput(r.json, opened)), response_ms: +r.ms.toFixed(1), response_without_layout_ms: +plain.ms.toFixed(1), response_bytes: r.bytes });
  }
}
for (const r of rows) console.log(JSON.stringify(r));
if (args.json) writeFileSync(args.json, JSON.stringify(rows, null, 1));
process.exit(rows.every((r) => r.ok) ? 0 : 1);
