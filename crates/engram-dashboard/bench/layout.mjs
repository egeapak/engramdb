// Graph layout benchmark: time, quality and determinism of every layout in
// web/js/graph/layouts.js (plus the reviewer's prototype as a baseline) on
// deterministic synthetic graphs. A dev tool, not part of the binary (see
// README.md).
//
//   node bench/layout.mjs                 # n = 60, 1000, 2000; every layout
//   node bench/layout.mjs --n 1000 --layouts scope-force --runs 9
//   node bench/layout.mjs --json          # one JSON row per (layout, n)
//   node bench/layout.mjs --graph g.json  # a saved /api/projects/{id}/graph response
//   LAYOUT_OPTIONS='{"topPull":0.03}' node bench/layout.mjs   # a style's layout.options
//
// Each row: cold ms (a fresh Node process, first call: what a page's worker
// pays), warm ms (median of --runs after a warm-up), crossings between
// straight relations, mean relation length / median nearest-neighbour gap,
// overlap (memories under 6 px apart at Fit), 5-NN same-scope share, labels
// placed, and whether a second run gave identical output.

import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { generate } from './data.mjs';
import { measure } from './metrics.mjs';
import { LAYOUTS } from '../web/js/graph/layouts.js';
import { forceScoped } from './prototype.mjs';

const SELF = fileURLToPath(import.meta.url);
const radiusOf = (nd) => 5 + Math.min(6, Math.sqrt(nd.reads || 0) * 0.55);

/** The view's layout input (views/graph.js) for a generated graph. */
export function toInput(g, name, aspect = 0.62) {
  const options = process.env.LAYOUT_OPTIONS ? JSON.parse(process.env.LAYOUT_OPTIONS) : {};
  const sizes = new Map();
  for (const nd of g.nodes) sizes.set(nd.top, (sizes.get(nd.top) || 0) + 1);
  const ranked = [...sizes.keys()].filter((k) => k).sort((a, b) => sizes.get(b) - sizes.get(a) || (a < b ? -1 : 1));
  const slot = new Map(ranked.map((k, q) => [k, q]));
  return {
    layout: name,
    options,
    nodes: g.nodes.map((nd) => ({ id: nd.id, top_scope: nd.top, slot: nd.top ? slot.get(nd.top) : null, scope: nd.scope, degree: nd.degree, r: radiusOf(nd), label: nd.label, weight: nd.reads })),
    edges: g.edges.map((e) => ({ s: e.s, t: e.t, kind: e.kind, far_scope: g.nodes[e.t].top })),
    aspect,
    width: 920,
  };
}

/** A saved `/graph` response as a generated graph (`n` in the table is its file). */
export function fromApi(api) {
  const index = new Map(api.nodes.map((nd, i) => [nd.id, i]));
  const reads = (nd) => nd.usage.returned + nd.usage.injected + nd.usage.fetched;
  const nodes = api.nodes.map((nd) => ({ id: nd.id, top: nd.top_scope || '', scope: nd.scope || '', reads: reads(nd), degree: 0, label: String(nd.summary || '').slice(0, 24) }));
  const edges = [];
  for (const e of api.edges) {
    if (!index.has(e.src) || !index.has(e.dst) || e.dst_store) continue;
    edges.push({ s: index.get(e.src), t: index.get(e.dst), kind: e.kind, status: e.status });
  }
  for (const e of edges) {
    nodes[e.s].degree++;
    nodes[e.t].degree++;
  }
  return { nodes, edges };
}

const graphOf = (spec) => (/^\d+$/.test(String(spec)) ? generate(Number(spec)) : fromApi(JSON.parse(readFileSync(spec, 'utf8'))));

/** Every layout under test: name → (graph) → { x, y, labels? } plus the raw output. */
const RUNNERS = {
  prototype: (g) => {
    const res = forceScoped(g, { aspect: 0.62 });
    return { x: res.x, y: res.y, raw: res };
  },
};
for (const name of Object.keys(LAYOUTS)) {
  RUNNERS[name] = (g) => {
    const res = LAYOUTS[name](toInput(g, name));
    const n = g.nodes.length;
    const x = new Float64Array(n);
    const y = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      x[i] = res.pos[2 * i];
      y[i] = res.pos[2 * i + 1];
    }
    return { x, y, raw: res };
  };
}

/** The contract in graph/layouts.js; returns a list of problems. */
function contract(g, raw) {
  if (!raw.pos) return ['(prototype: no contract)'];
  const n = g.nodes.length;
  const bad = [];
  if (raw.pos.length !== 2 * n || raw.pos.some((v) => !Number.isFinite(v))) bad.push('pos');
  if (!Array.isArray(raw.routes) || raw.routes.length !== g.edges.length || raw.routes.some((r) => r.length < 2 || r.some((p) => !Number.isFinite(p[0]) || !Number.isFinite(p[1])))) bad.push('routes');
  if (!Array.isArray(raw.labels) || raw.labels.length !== n || raw.labels.some((l) => !l || typeof l.placed !== 'boolean')) bad.push('labels');
  const b = raw.bounds;
  if (!b || ![b.x, b.y, b.w, b.h].every(Number.isFinite)) bad.push('bounds');
  return bad;
}

function median(xs) {
  const s = [...xs].sort((a, b) => a - b);
  return s[s.length >> 1];
}

function coldOnce(name, n) {
  const out = spawnSync(process.execPath, [SELF, '--cold-one', name, String(n)], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(out.stderr);
  return Number(out.stdout.trim());
}

function main(argv) {
  const arg = (flag, dflt) => {
    const i = argv.indexOf(flag);
    return i >= 0 ? argv[i + 1] : dflt;
  };
  if (argv[0] === '--cold-one') {
    const g = graphOf(argv[2]);
    const t0 = performance.now();
    RUNNERS[argv[1]](g);
    console.log((performance.now() - t0).toFixed(1));
    return;
  }
  const ns = arg('--graph', null) ? arg('--graph').split(',') : arg('--n', '60,1000,2000').split(',');
  const names = arg('--layouts', Object.keys(RUNNERS).join(',')).split(',');
  const runs = Number(arg('--runs', '5'));
  const colds = Number(arg('--cold', '3'));
  const json = argv.includes('--json');
  const rows = [];
  for (const n of ns) {
    const g = graphOf(n);
    for (const name of names) {
      if (!RUNNERS[name]) throw new Error(`unknown layout ${name} (have ${Object.keys(RUNNERS)})`);
      const cold = colds ? median(Array.from({ length: colds }, () => coldOnce(name, n))) : null;
      RUNNERS[name](g); // warm-up
      const times = [];
      let res = null;
      for (let k = 0; k < runs; k++) {
        const t0 = performance.now();
        res = RUNNERS[name](g);
        times.push(performance.now() - t0);
      }
      const again = RUNNERS[name](g);
      const same = JSON.stringify(again.raw.pos || [...again.x, ...again.y]) === JSON.stringify(res.raw.pos || [...res.x, ...res.y]) && JSON.stringify(again.raw.routes || null) === JSON.stringify(res.raw.routes || null);
      const m = measure(g, res);
      const labels = res.raw.labels ? res.raw.labels.filter((l) => l.placed).length : null;
      rows.push({ layout: name, n: g.nodes.length, input: n, edges: g.edges.length, cold, warm: median(times), ...m, labels, deterministic: same, contract: contract(g, res.raw) });
    }
  }
  if (json) {
    for (const r of rows) console.log(JSON.stringify(r));
    return;
  }
  const f = (v, d = 0) => (v === null || v === undefined ? '-' : v.toFixed(d));
  console.log('| layout | n | cold ms | warm ms | crossings | edge/NN | overlap % | same scope % | labels placed | deterministic | contract |');
  console.log('|---|---|---|---|---|---|---|---|---|---|---|');
  for (const r of rows) {
    console.log(`| ${r.layout} | ${r.n} | ${f(r.cold)} | ${f(r.warm)} | ${r.crossings} | ${f(r.meanEdgeNN, 1)} | ${f(r.overlapPct, 1)} | ${f(r.scopePurity)} | ${r.labels === null ? '-' : `${r.labels}/${r.n}`} | ${r.deterministic ? 'yes' : 'NO'} | ${r.contract.length ? r.contract.join(' ') : 'ok'} |`);
  }
}

// Run only as a script, so another tool can import toInput / fromApi.
if (process.argv[1] === SELF) main(process.argv.slice(2));
