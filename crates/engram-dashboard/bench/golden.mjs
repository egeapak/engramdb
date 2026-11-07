// Golden fixtures for the server's layout ports (src/layout/): a few
// deterministic /graph-shaped responses, laid out by the page's own JS
// (graph/input.js builds the input, scope-force.js / scope-overview.js lay
// it out), written with the JS output to src/layout/fixtures/. The Rust test
// `the_ports_match_the_js_on_the_golden_fixtures` builds the same inputs
// from the same responses with its ports and compares every double bit for
// bit (doubles are stored as base64 little-endian, so no JSON parse can
// round them).
//
//   node bench/golden.mjs          # rewrite the fixtures
//   node bench/golden.mjs --check  # exit 1 if the committed ones differ
//
// Regenerate after changing a layout JS file (and its Rust port); the pin
// test (`the_layout_js_is_pinned_to_the_layout_version`) says when.

import { readFileSync, writeFileSync } from 'node:fs';
import { forceInput, overviewInput, inputCheck, overviewCheck, scopePairs } from '../web/js/graph/input.js';
import { LAYOUTS } from '../web/js/graph/layouts.js';
import { SERVER_LAYOUT_VERSION } from '../web/js/graph/server-layout.js';

const OUT = new URL('../src/layout/fixtures/', import.meta.url);

/** A seeded generator, so every run writes the same fixtures. */
function rng(seed) {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

const SUMMARIES = [
  'Use flock for the write lock: it is advisory',
  'A rule, with a comma, and more words after it',
  'Short one',
  'averyveryverylongwordwithoutanyspaceatall and more',
  'Ünïcödé summary — with a dash (and parens)',
  'Emoji 😀 in the summary. Then a sentence',
  '  padded   summary  with   spaces  ',
  'Never - cut here because - is spaced',
  'x',
  'The daemon reads a project directory for exactly one op',
];

function b64(nums) {
  const buf = Buffer.alloc(8 * nums.length);
  nums.forEach((v, i) => buf.writeDoubleLE(v, 8 * i));
  return buf.toString('base64');
}

/** A /graph-shaped response: `tops` top scopes, sub-scopes, stubs. */
function graph(seed, n, tops, subs, edgesPer) {
  const rand = rng(seed);
  const pick = (a) => a[Math.floor(rand() * a.length)];
  const nodes = [];
  for (let i = 0; i < n; i++) {
    const top = pick(tops);
    const sub = rand() < 0.75 ? pick(subs) : null;
    const scope = top ? (sub ? `${top}.${sub}${rand() < 0.3 ? '.deep' : ''}` : top) : null;
    const id = ((i * 2654435761) >>> 0).toString(16).padStart(8, '0') + (i * 7919).toString(16).padStart(8, '0');
    const node = { id, summary: `${pick(SUMMARIES)} ${i}`, usage: { returned: Math.floor(rand() * 9), injected: Math.floor(rand() * 4), fetched: Math.floor(rand() * 30 * rand()) }, degree: 0 };
    if (top !== null) node.top_scope = top;
    if (scope) node.scope = scope;
    if (rand() < 0.9) node.color_slot = Math.floor(rand() * 12);
    nodes.push(node);
  }
  const edges = [];
  const m = Math.floor(n * edgesPer);
  for (let k = 0; k < m; k++) {
    const s = Math.floor(rand() * n);
    const u = rand();
    let e;
    if (u < 0.06) e = { src: nodes[s].id, dst: 'ffffffffffff' + k.toString(16).padStart(4, '0') }; // dangling
    else if (u < 0.1) e = { src: nodes[s].id, dst: nodes[Math.floor(rand() * n)].id, dst_store: 'otherstore' }; // cross-store
    else {
      // Mostly near (same block of ids), sometimes anywhere.
      const t = rand() < 0.7 ? (s + 1 + Math.floor(rand() * 12)) % n : Math.floor(rand() * n);
      if (t === s) continue;
      e = { src: nodes[s].id, dst: nodes[t].id };
    }
    e.kind = pick(['related_to', 'supports', 'contradicts', 'derived_from']);
    edges.push(e);
  }
  const byId = new Map(nodes.map((nd, i) => [nd.id, i]));
  for (const e of edges) {
    nodes[byId.get(e.src)].degree++;
    if (!e.dst_store && byId.has(e.dst)) nodes[byId.get(e.dst)].degree++;
  }
  return { nodes, edges };
}

function forceFixture(g, groups, aspect20, width) {
  const { input } = forceInput(g, { layout: 'scope-force', options: {}, opened: groups === 'sub', aspect: aspect20 / 20, width });
  const out = LAYOUTS['scope-force'](input);
  return {
    v: SERVER_LAYOUT_VERSION,
    layout: 'scope-force',
    groups,
    aspect20,
    width,
    check: inputCheck(input),
    nodes: g.nodes.map((nd) => ({ id: nd.id, top_scope: nd.top_scope ?? null, scope: nd.scope ?? null, summary: nd.summary, reads: nd.usage.returned + nd.usage.injected + nd.usage.fetched, degree: nd.degree, color_slot: nd.color_slot ?? null })),
    edges: g.edges.map((e) => ({ src: e.src, dst: e.dst, dst_store: e.dst_store ?? null })),
    out: {
      pos: b64(out.pos),
      routes: b64(out.routes.flat(2)),
      labels: b64(out.labels.flatMap((l) => [l.x, l.y, l.w])),
      anchors: out.labels.map((l) => l.anchor[0] + (l.placed ? '+' : '-')).join(''),
      bounds: b64([out.bounds.x, out.bounds.y, out.bounds.w, out.bounds.h]),
      region_r: b64([out.region_r]),
      scopes: b64(out.scopes.flatMap((s) => [s.x, s.y, s.w, s.size])),
      scope_flags: out.scopes.map((s) => `${s.key}:${s.placed ? 1 : 0}${s.backed ? 1 : 0}`),
    },
  };
}

function overviewFixture(seed, aspect20, width) {
  const rand = rng(seed);
  const names = ['storage', 'dashboard', 'retrieval', '', 'hooks', 'a', 'a-rather-long-scope-name-for-labels', 'ops', 'mcp', 'daemon', 'Ünïcödé', 'scoring', 'tests', 'fuzz'];
  const scopes = names.map((name, i) => ({ name, count: i === 13 ? 0 : Math.floor(2000 / (i + 1) ** 1.3 * (0.5 + rand())) }));
  const rows = [];
  for (let k = 0; k < 40; k++) {
    const a = names[Math.floor(rand() * names.length)];
    const b = rand() < 0.15 ? a : names[Math.floor(rand() * names.length)];
    rows.push({ a, b, count: 1 + Math.floor(rand() * 60 * rand()) });
  }
  // The page's own pairing (graph/input.js scopePairs, as
  // views/graph-overview.js calls it), not a copy.
  const shown = scopes.filter((l) => l.count > 0);
  const index = new Map(shown.map((l, i) => [l.name, i]));
  const pairs = scopePairs(rows).filter((p) => index.has(p.a) && index.has(p.b));
  const input = overviewInput(shown, pairs.map((p) => ({ a: p.a, b: p.b, n: p.n })), width, aspect20 / 20);
  const out = LAYOUTS['scope-overview'](input);
  return {
    v: SERVER_LAYOUT_VERSION,
    layout: 'scope-overview',
    aspect20,
    width,
    scopes,
    scope_edges: rows,
    check: overviewCheck(input),
    out: {
      pos: b64(out.pos),
      r: b64(out.r),
      routes: b64(out.routes.flat(2)),
      labels: b64(out.labels.flatMap((l) => [l.x, l.y, l.w])),
      label_flags: out.labels.map((l) => `${l.anchor[0]}${l.placed ? 1 : 0}${l.short ? 1 : 0}${l.forced ? 1 : 0}`).join(''),
      bounds: b64([out.bounds.x, out.bounds.y, out.bounds.w, out.bounds.h]),
    },
  };
}

const fixtures = {
  // Plain's graph: several top scopes and no scope, labels nudged (≤ 300).
  'force-top': forceFixture(graph(7, 140, ['storage', 'dashboard', 'retrieval', '', 'hooks', 'a-long-scope-name'], ['lock', 'index', 'graph', 'x'], 1.6), 'top', 12, 520),
  // An opened scope: one top scope by sub-scope, stubs out of it, no
  // nudging (> 300), a phone-width box.
  'force-sub': forceFixture(graph(11, 320, ['storage'], ['lock', 'index', 'graph', 'manifest', 'edges', 'x'], 1.3), 'sub', 32, 360),
  overview: overviewFixture(5, 11, 1240),
};

let differs = false;
for (const [name, fx] of Object.entries(fixtures)) {
  const file = new URL(`${name}.json`, OUT);
  const text = JSON.stringify(fx) + '\n';
  if (process.argv.includes('--check')) {
    let old = '';
    try {
      old = readFileSync(file, 'utf8');
    } catch (_) {}
    if (old !== text) {
      differs = true;
      console.error(`${name}: differs from the committed fixture`);
    }
  } else {
    writeFileSync(file, text);
    console.log(`${name}: ${text.length} bytes, ${fx.nodes ? fx.nodes.length : fx.scopes.length} nodes`);
  }
}
process.exit(differs ? 1 : 0);
