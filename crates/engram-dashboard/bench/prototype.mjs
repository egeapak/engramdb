// The reviewer's `force-scoped` prototype (graph view v1.1 layout review),
// kept verbatim as the baseline the shipped scope-force layout is measured
// against. Not shipped: bench/ is outside web/, so build.rs never embeds it.

import { cmp, radiusOf, force, collide, sunflower } from './prototype-common.mjs';

function base(nodes) {
  const n = nodes.length;
  const r = new Float64Array(n);
  nodes.forEach((nd, i) => (r[i] = radiusOf(nd)));
  return { n, x: new Float64Array(n), y: new Float64Array(n), r };
}

const byId = (nodes) => (a, b) => cmp(nodes[a].id, nodes[b].id);

export function forceScoped(g, opt = {}) {
  const { nodes, edges } = g;
  const { n, x, y, r } = base(nodes);
  const aspect = opt.aspect || 0.62;
  const all = [...Array(n).keys()];
  const byTop = new Map();
  const byLeaf = new Map();
  for (const i of [...all].sort(byId(nodes))) {
    const t = nodes[i].top || '';
    const l = nodes[i].scope || '';
    if (!byTop.has(t)) byTop.set(t, []);
    byTop.get(t).push(i);
    if (!byLeaf.has(l)) byLeaf.set(l, []);
    byLeaf.get(l).push(i);
  }
  // seed: each top scope on its own sunflower, tops on a larger sunflower by size
  const tops = [...byTop.keys()].sort((a, b) => byTop.get(b).length - byTop.get(a).length || cmp(a, b));
  const sp = 20;
  tops.forEach((t, k) => {
    const rr = sp * Math.sqrt(n) * 0.55 * Math.sqrt(k / tops.length) * 1.6;
    const cx = rr * Math.cos(k * 2.399963) / Math.sqrt(aspect);
    const cy = rr * Math.sin(k * 2.399963) * Math.sqrt(aspect);
    const ids = [...byTop.get(t)].sort((a, b) => cmp(nodes[a].scope, nodes[b].scope) || cmp(nodes[a].id, nodes[b].id));
    sunflower(ids, x, y, cx, cy, sp * 0.8);
  });
  const groups = [];
  for (const [t, ids] of byTop) if (t) groups.push([ids, opt.topPull ?? 0.025]);
  for (const [l, ids] of byLeaf) if (l && l.includes('.')) groups.push([ids, opt.leafPull ?? 0.02]);
  force(all, edges.map((e) => [e.s, e.t, 1]), x, y, { k: 30, gravity: opt.gravity ?? 0.01, cutoff: 30 * 6, cx: 0, cy: 0, aspectX: aspect, aspectY: 1, groups, iters: n > 400 ? 200 : 300 });
  collide(all, x, y, r, 5);
  // scope labels at each top scope's centroid (drawn faint, under the graph)
  const out = [];
  for (const t of tops) {
    if (!t) continue;
    const ids = byTop.get(t);
    // at the centroid of the scope's densest part: the median point
    const xs = ids.map((i) => x[i]).sort((a, b) => a - b);
    const ys = ids.map((i) => y[i]).sort((a, b) => a - b);
    out.push({ kind: 'label', x: xs[xs.length >> 1], y: ys[ys.length >> 1], label: t, top: t });
  }
  return { x, y, r, groups: out };
}
