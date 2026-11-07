// Deterministic synthetic memory graph: ~10 top scopes with 2-level nesting,
// skewed (Zipf-like) sizes, ~0.7 edges per node, mostly within a scope, with
// cross-scope edges concentrated on a few scope pairs (affinity).
// Kinds: related_to most common, then supports, contradicts rare; ~20% proposed.

export function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const TOPS = [
  ['storage', ['index', 'paths', 'migration', 'lock']],
  ['retrieval', ['engine', 'rerank', 'filter']],
  ['daemon', ['socket', 'cache', 'spawn']],
  ['harvest', ['ledger', 'index', 'pin']],
  ['cli', ['output', 'hooks']],
  ['mcp', ['server', 'startup']],
  ['scoring', ['decay', 'signals']],
  ['dashboard', ['graph', 'security']],
  ['models', ['onnx', 'nli']],
  ['ci', []],
];
const KINDS = ['decision', 'hazard', 'convention', 'context', 'debug', 'pattern'];
const WORDS = ['index', 'lock', 'vector', 'schema', 'retry', 'socket', 'token', 'prune', 'cache', 'rank', 'hook', 'pool', 'ledger', 'digest', 'budget', 'edge', 'decay', 'filter'];

const EDGE_KINDS = [
  ['related_to', 0.5],
  ['supports', 0.2],
  ['derived_from', 0.1],
  ['supersedes', 0.07],
  ['duplicates', 0.07],
  ['contradicts', 0.06],
];

export function generate(n, seed = 7, relations = Math.round(n * 0.7)) {
  const rand = prng(seed * 1000003 + n);
  const pick = (arr) => arr[Math.floor(rand() * arr.length)];
  // Zipf weights over top scopes, skewed.
  const w = TOPS.map((_, i) => 1 / Math.pow(i + 1, 0.9));
  const wsum = w.reduce((a, b) => a + b, 0);
  const nodes = [];
  for (let i = 0; i < n; i++) {
    let scope = '';
    if (rand() > 0.05) {
      let r = rand() * wsum;
      let t = 0;
      while (r > w[t]) r -= w[t++];
      const [top, subs] = TOPS[t];
      // 30% sit on the top scope itself, the rest on a sub-scope (skewed).
      if (subs.length && rand() > 0.3) {
        const sw = subs.map((_, k) => 1 / (k + 1));
        let q = rand() * sw.reduce((a, b) => a + b, 0);
        let k = 0;
        while (q > sw[k]) q -= sw[k++];
        scope = top + '.' + subs[k];
      } else scope = top;
    }
    const id = (Math.floor(rand() * 0xffffffff) >>> 0).toString(16).padStart(8, '0') + i.toString(16).padStart(4, '0');
    const reads = Math.floor(Math.pow(rand(), 3) * 60);
    nodes.push({
      id,
      scope,
      top: scope.split('.')[0],
      kind: pick(KINDS),
      criticality: Math.round(rand() * 10) / 10,
      reads,
      invalidated: rand() < 0.03,
      label: pick(WORDS) + ' ' + pick(WORDS) + ' ' + pick(['rule', 'note', 'fix', 'trap', 'choice']),
    });
  }
  // Affinity between top scopes: a few strong pairs.
  const tops = TOPS.map((t) => t[0]);
  const aff = new Map();
  const pairs = [
    ['storage', 'retrieval', 4], ['retrieval', 'scoring', 4], ['daemon', 'models', 3], ['harvest', 'storage', 3],
    ['cli', 'mcp', 2], ['mcp', 'daemon', 2], ['dashboard', 'storage', 1.5], ['models', 'retrieval', 2], ['ci', 'cli', 1],
  ];
  for (const [a, b, s] of pairs) { aff.set(a + '|' + b, s); aff.set(b + '|' + a, s); }
  const byTop = new Map();
  const byScope = new Map();
  nodes.forEach((nd, i) => {
    if (!byTop.has(nd.top)) byTop.set(nd.top, []);
    byTop.get(nd.top).push(i);
    if (!byScope.has(nd.scope)) byScope.set(nd.scope, []);
    byScope.get(nd.scope).push(i);
  });
  // Edge sources: a skewed subset (hubs), so many memories stay isolated.
  const m = Math.min(relations, (n * (n - 1)) >> 1);
  const edges = [];
  const seen = new Set();
  let guard = 0;
  while (edges.length < m && guard++ < m * 20) {
    const s = Math.floor(Math.pow(rand(), 1.6) * n);
    const src = nodes[s];
    let t;
    const r = rand();
    if (r < 0.45 && byScope.get(src.scope).length > 1) t = pick(byScope.get(src.scope));
    else if (r < 0.75 && byTop.get(src.top).length > 1) t = pick(byTop.get(src.top));
    else {
      // cross-scope, weighted by affinity
      const cands = tops.filter((x) => x !== src.top && byTop.has(x));
      const ws = cands.map((x) => (aff.get(src.top + '|' + x) || 0.15));
      let q = rand() * ws.reduce((a, b) => a + b, 0);
      let k = 0;
      while (q > ws[k]) q -= ws[k++];
      t = pick(byTop.get(cands[k] || cands[0]));
    }
    if (t === s) continue;
    const key = Math.min(s, t) + ',' + Math.max(s, t);
    if (seen.has(key)) continue;
    seen.add(key);
    let q = rand();
    let kind = EDGE_KINDS[0][0];
    for (const [k, p] of EDGE_KINDS) { if (q < p) { kind = k; break; } q -= p; }
    edges.push({ s, t, kind, status: rand() < 0.2 ? 'proposed' : 'confirmed' });
  }
  const degree = new Array(n).fill(0);
  for (const e of edges) { degree[e.s]++; degree[e.t]++; }
  nodes.forEach((nd, i) => (nd.degree = degree[i]));
  return { nodes, edges };
}

export const DIRECTED = new Set(['supports', 'derived_from', 'supersedes']);
