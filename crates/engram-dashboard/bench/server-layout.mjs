// Parity and size check of the server's layouts (src/layout/) against the
// page's JS layouts, on a running dashboard (embedded assets, or an
// `--assets-dir` whose layout JS is the binary's: otherwise the server
// sends no layout):
//
//   node bench/server-layout.mjs --url 'http://127.0.0.1:7341/#token=…' \
//     [--projects id,id] [--boxes 0.6x1280,1.6x360] [--scope <top scope>] [--json out.json]
//
// For every available project and box (aspect × width, as the page rounds
// them; the default is a 1280 px desktop box and a 390 px phone's) it asks
// /graph for what the page asks (the overview's summary, a scope opened —
// `--scope`, else the store's largest — and "show all") with a server
// layout, builds the input from the same response with the page's own
// graph/input.js, runs the JS layout in this process and compares every
// key of both outputs bit for bit (Object.is on every number, the same keys
// on both sides): `max_delta` is 0 only when they are the same bits. It
// also reports labels placed and relation crossings for each, JS time
// (cold = first call in this process, warm = median of 5), the response
// time with and without `layout=` (start the dashboard with
// ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0 for a cold server figure), and the
// encoded layout's size against alternative encodings, raw and gzipped.
// Exits 1 when any layout differs in any bit, is missing or fails to decode.

import { gzipSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { LAYOUTS } from '../web/js/graph/layouts.js';
import { fromServer } from '../web/js/graph/server-layout.js';
import { forceInput, overviewInput } from '../web/js/graph/input.js';
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
// The page's boxes at a 1280 × 860 desktop window and a 390 × 844 phone
// (measured with views/graph.js measureBox).
const boxes = (args.boxes || '0.6x1280,1.6x360').split(',').map((b) => b.split('x').map(Number));
const OVERVIEW_MIN = 150;

async function get(path) {
  const t0 = performance.now();
  const res = await fetch(origin + path, { headers: { Authorization: `Bearer ${token}` } });
  const text = await res.text();
  const ms = performance.now() - t0;
  if (!res.ok) throw new Error(`${path}: ${res.status} ${text}`);
  return { json: JSON.parse(text), ms, bytes: text.length };
}

/** The overview's input for a summary, as views/graph-overview.js builds it. */
function overviewOf(g, aspect, width) {
  const scopes = g.scopes.filter((l) => l.count > 0);
  const index = new Map(scopes.map((l, i) => [l.name, i]));
  const pairs = scopePairs(g)
    .filter((p) => index.has(p.a) && index.has(p.b))
    .map((p) => ({ a: p.a, b: p.b, n: p.tally.count }));
  return overviewInput(scopes, pairs, width, aspect);
}

/**
 * The largest difference between two layout outputs over the union of
 * their keys: 0 only when every number is the same double (Object.is, so
 * -0 and 0 differ) and every other value is equal; Infinity for a key on
 * one side only, a length mismatch or a NaN.
 */
function maxDelta(a, b) {
  let d = 0;
  const num = (x, y) => {
    if (typeof x === 'number' && typeof y === 'number') {
      if (Object.is(x, y)) return;
      const v = Math.abs(x - y);
      // -0 against 0 is a (tiny) difference too.
      d = Math.max(d, Number.isNaN(v) ? Infinity : v || Number.MIN_VALUE);
    } else if (Array.isArray(x) && Array.isArray(y)) {
      if (x.length !== y.length) d = Infinity;
      else for (let i = 0; i < x.length; i++) num(x[i], y[i]);
    } else if (x && y && typeof x === 'object' && typeof y === 'object') {
      // `box` is the JS's internal label / name rectangle, which the page
      // does not read and the server does not send.
      const keys = new Set([...Object.keys(x), ...Object.keys(y)]);
      keys.delete('box');
      for (const k of keys) {
        if (!(k in x) || !(k in y)) d = Infinity;
        else num(x[k], y[k]);
      }
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

function check(label, L, input, groups) {
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
  const srv = fromServer(L, input, groups);
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
  row.ok = row.max_delta === 0;
  return row;
}

const q = (o) => Object.entries(o).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');
const base = { include_proposed: 'true', include_rejected: 'true', include_invalidated: 'false' };

const rows = [];
const projects = (await get('/api/projects')).json.projects.filter((p) => p.available && (!args.projects || args.projects.split(',').includes(p.id)));
for (const p of projects) {
  const path = `/api/projects/${p.id}/graph?`;
  for (const [aspect, width] of boxes) {
    const lay = (names, extra = {}) => ({ layout: names, layout_aspect: aspect, layout_width: width, ...extra });
    const box = `${aspect}x${width}`;
    const scopeForce = (g, opened) => forceInput(g, { layout: 'scope-force', options: {}, opened, aspect, width }).input;
    // Tab click: the overview's summary (or the whole graph at ≤ 150).
    const ov = await get(path + q({ ...base, summary_over: OVERVIEW_MIN, ...lay('scope-overview,scope-force') }));
    const ovPlain = await get(path + q({ ...base, summary_over: OVERVIEW_MIN }));
    const tab = ov.json.summary ? check('tab (overview)', ov.json.layout, overviewOf(ov.json, aspect, width)) : check('tab (whole graph)', ov.json.layout, scopeForce(ov.json, false), 'top');
    rows.push({ project: p.name, box, ...tab, response_ms: +ov.ms.toFixed(1), response_without_layout_ms: +ovPlain.ms.toFixed(1), response_bytes: ov.bytes });
    if (!ov.json.summary) continue;
    // A scope opened (`--scope`, else the largest), and show all.
    const scope = args.scope || [...ov.json.scopes].filter((l) => l.name).sort((x, y) => y.count - x.count)[0]?.name;
    const cases = [['show all', { ...base, ...lay('scope-force') }, false]];
    if (scope) cases.unshift([`open ${scope}`, { ...base, top_scope: scope, ...lay('scope-force,opened', { layout_open_over: OVERVIEW_MIN }) }, true]);
    for (const [label, params, opened] of cases) {
      const r = await get(path + q(params));
      const plain = await get(path + q(Object.fromEntries(Object.entries(params).filter(([k]) => !k.startsWith('layout')))));
      rows.push({ project: p.name, box, ...check(label, r.json.layout, scopeForce(r.json, opened), opened ? 'sub' : 'top'), response_ms: +r.ms.toFixed(1), response_without_layout_ms: +plain.ms.toFixed(1), response_bytes: r.bytes });
    }
  }
}
for (const r of rows) console.log(JSON.stringify(r));
if (args.json) writeFileSync(args.json, JSON.stringify(rows, null, 1));
process.exit(rows.every((r) => r.ok) ? 0 : 1);
