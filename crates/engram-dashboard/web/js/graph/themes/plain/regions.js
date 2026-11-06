// Plain's scope regions, shared by its SVG backdrop (./index.js) and its
// canvas hooks (./canvas.js), so the two drawings can never place a region
// differently.

import { radius } from '../../radius.js';

/** A scope's texture past the seventh scope: 0 none, 1 hatch, 2 dots, 3 cross-hatch (index.js docs). */
export const texture = (n) => (n === null || n === undefined ? 0 : Math.min(3, Math.floor(n / 7)));

/**
 * Each named top scope's region, sorted by name: `{ key, slot, discs }`,
 * `discs` a flat [x, y, q, …] of a disc around each of its memories
 * (`lay.region_r`, but at least the memory's own radius + 6, so a large
 * memory is never wider than its region).
 */
export function regionDiscs(g, lay) {
  const rr = lay.region_r || 24;
  const by = new Map();
  g.nodes.forEach((n, i) => {
    if (!n.top_scope) return;
    if (!by.has(n.top_scope)) by.set(n.top_scope, { slot: n.color_slot, discs: [] });
    by.get(n.top_scope).discs.push(lay.pos[2 * i], lay.pos[2 * i + 1], Math.max(radius(n) + 6, rr));
  });
  return [...by].sort((p, q) => (p[0] < q[0] ? -1 : p[0] > q[0] ? 1 : 0)).map(([key, a]) => ({ key, slot: a.slot, discs: a.discs }));
}

/**
 * `regionDiscs` as SVG path data: `{ key, slot, d }`, one path per scope,
 * every disc drawn the same way round, so overlapping discs fill once
 * (nonzero) and never darken.
 */
export function regionArcs(g, lay) {
  return regionDiscs(g, lay).map(({ key, slot, discs }) => {
    let d = '';
    for (let j = 0; j < discs.length; j += 3) {
      const x = discs[j];
      const y = discs[j + 1];
      const q = discs[j + 2];
      const qs = q.toFixed(1);
      d += `M${(x - q).toFixed(1)},${y.toFixed(1)}a${qs},${qs} 0 1,0 ${(2 * q).toFixed(1)},0a${qs},${qs} 0 1,0 ${(-2 * q).toFixed(1)},0`;
    }
    return { key, slot, d };
  });
}
