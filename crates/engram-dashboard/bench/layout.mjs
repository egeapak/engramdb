// Graph layout quality bench: the server's layouts (src/layout/ in the
// dashboard crate) as a running dashboard sends them, against the
// reviewer's prototype as a baseline on deterministic synthetic graphs. A
// dev tool, not part of the binary (see README.md).
//
//   node bench/layout.mjs                                  # the prototype on n = 60, 1000, 2000, 300x8000, 2000x10000
//   node bench/layout.mjs --graph a.json,b.json            # saved /graph responses that carry a `layout`
//   node bench/layout.mjs --url '<token URL>' [--projects id,id] [--box 0.6x1280]
//   node bench/layout.mjs --json                           # one JSON row per (layout, graph)
//
// Each row: crossings between straight relations, mean relation length /
// median nearest-neighbour gap, overlap (memories under 6 px apart at Fit),
// 5-NN same-scope share, labels placed, whether the output keeps the
// contract (graph/layouts.js), and for --url whether a second request gave
// the same layout and how long /graph took with and without `layout=`. The
// server's time is the Criterion bench's (benches/dashboard_graph.rs).

import { readFileSync } from 'node:fs';
import { generate } from './data.mjs';
import { measure } from './metrics.mjs';
import { decodeForce, decodeGrid } from '../web/js/graph/server-layout.js';
import { forceScoped } from './prototype.mjs';

/** A `/graph` response as the metrics' graph: node indices, drawn relations only. */
export function fromApi(api) {
  const index = new Map(api.nodes.map((nd, i) => [nd.id, i]));
  const nodes = api.nodes.map((nd) => ({ id: nd.id, top: nd.top_scope || '', scope: nd.scope || '', degree: 0 }));
  const edges = [];
  const kept = [];
  api.edges.forEach((e, k) => {
    if (!index.has(e.src) || !index.has(e.dst) || e.dst_store) return;
    edges.push({ s: index.get(e.src), t: index.get(e.dst), kind: e.kind, status: e.status });
    kept.push(k);
  });
  for (const e of edges) {
    nodes[e.s].degree++;
    nodes[e.t].degree++;
  }
  return { nodes, edges, kept };
}

/** The server's layout in `api` (a /graph response), decoded as the page does. */
function decoded(api) {
  const L = api.layout;
  if (!L) throw new Error('the response carries no layout (ask with layout=…&layout_aspect=…&layout_width=…)');
  if (L.name === 'grid') return decodeGrid(L, api);
  return decodeForce(L, api, L.groups);
}

/** The contract in graph/layouts.js; returns a list of problems. */
function contract(api, raw) {
  const n = api.nodes.length;
  const bad = [];
  if (raw.pos.length !== 2 * n || raw.pos.some((v) => !Number.isFinite(v))) bad.push('pos');
  if (!Array.isArray(raw.routes) || raw.routes.length !== api.edges.length || raw.routes.some((r) => r.length < 2 || r.some((p) => !Number.isFinite(p[0]) || !Number.isFinite(p[1])))) bad.push('routes');
  if (!Array.isArray(raw.labels) || raw.labels.length !== n || raw.labels.some((l) => !l || typeof l.placed !== 'boolean')) bad.push('labels');
  const b = raw.bounds;
  if (!b || ![b.x, b.y, b.w, b.h].every(Number.isFinite)) bad.push('bounds');
  return bad;
}

function row(name, input, api, extra = {}) {
  const g = fromApi(api);
  const lay = decoded(api);
  const n = g.nodes.length;
  const x = new Float64Array(n);
  const y = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    x[i] = lay.pos[2 * i];
    y[i] = lay.pos[2 * i + 1];
  }
  const m = measure(g, { x, y });
  const labels = lay.labels.filter((l) => l.placed).length;
  return { layout: name, n, input, edges: g.edges.length, ...m, labels, contract: contract(api, lay), ...extra };
}

function prototypeRows(ns) {
  return ns.map((spec) => {
    const mm = /^(\d+)(?:x(\d+))?$/.exec(spec);
    const g = mm[2] ? generate(Number(mm[1]), 7, Number(mm[2])) : generate(Number(mm[1]));
    const res = forceScoped(g, { aspect: 0.62 });
    return { layout: 'prototype', n: g.nodes.length, input: spec, edges: g.edges.length, ...measure(g, res), labels: null, contract: [] };
  });
}

async function urlRows(url, projects, box) {
  const origin = new URL(url).origin;
  const token = url.split('#token=')[1];
  const get = async (path) => {
    const t0 = performance.now();
    const res = await fetch(origin + path, { headers: { Authorization: `Bearer ${token}` } });
    const body = await res.json();
    if (!res.ok) throw new Error(`${path}: ${res.status} ${JSON.stringify(body)}`);
    return { body, ms: performance.now() - t0 };
  };
  const [aspect, width] = box.split('x');
  const ids = projects ? projects.split(',') : (await get('/api/projects')).body.projects.filter((p) => p.available).map((p) => p.id);
  const rows = [];
  for (const id of ids) {
    const base = `/api/projects/${id}/graph?include_proposed=true&include_rejected=true`;
    const plain = await get(base);
    for (const name of ['scope-force', 'grid']) {
      const q = `${base}&layout=${name}&layout_aspect=${aspect}&layout_width=${width}`;
      const a = await get(q);
      const b = await get(q);
      rows.push(row(name, id, a.body, { same: JSON.stringify(a.body.layout) === JSON.stringify(b.body.layout), ms: a.ms, plain_ms: plain.ms }));
    }
  }
  return rows;
}

async function main(argv) {
  const arg = (flag, dflt) => {
    const i = argv.indexOf(flag);
    return i >= 0 ? argv[i + 1] : dflt;
  };
  let rows;
  if (arg('--url', null)) rows = await urlRows(arg('--url'), arg('--projects', null), arg('--box', '0.6x1280'));
  else if (arg('--graph', null)) rows = arg('--graph').split(',').map((f) => {
    const api = JSON.parse(readFileSync(f, 'utf8'));
    return row(api.layout ? api.layout.name : '?', f, api);
  });
  else rows = prototypeRows(arg('--n', '60,1000,2000,300x8000,2000x10000').split(','));
  if (argv.includes('--json')) {
    for (const r of rows) console.log(JSON.stringify(r));
    return;
  }
  const f = (v, d = 0) => (v === null || v === undefined ? '-' : v.toFixed(d));
  console.log('| layout | graph | n | relations | crossings | edge/NN | overlap % | same scope % | labels placed | contract | same again | ms (without layout) |');
  console.log('|---|---|---|---|---|---|---|---|---|---|---|---|');
  let failed = false;
  for (const r of rows) {
    if (r.contract.length || r.same === false) failed = true;
    console.log(`| ${r.layout} | ${r.input} | ${r.n} | ${r.edges} | ${r.crossings} | ${f(r.meanEdgeNN, 1)} | ${f(r.overlapPct, 1)} | ${f(r.scopePurity)} | ${r.labels === null ? '-' : `${r.labels}/${r.n}`} | ${r.contract.length ? r.contract.join(' ') : 'ok'} | ${r.same === undefined ? '-' : r.same ? 'yes' : 'NO'} | ${r.ms === undefined ? '-' : `${f(r.ms)} (${f(r.plain_ms)})`} |`);
  }
  if (failed) process.exitCode = 1;
}

await main(process.argv.slice(2));
