// An opened scope (`top_scope=<name>` on the graph view): its memories laid
// out by the core scope-force layout with each sub-scope as a region, its own
// relations drawn whole, and every relation leaving it collapsed into a
// faint stub from the memory to one tag per far scope ("storage · 12 ⚠ 2").
// A core render path: neutral words, neutral CSS (`.xs-*` / `.sub-*`).
//
// The tags sit around the laid-out memories, each in the direction its
// scope lies on the scope overview (the same layout, so opening a scope
// keeps the map's orientation), pushed apart along the ring when two would
// overlap. Pure geometry here; views/graph.js draws and wires them.

import { s } from '../dom.js';

/**
 * The region a memory of an opened scope belongs to: its sub-scope (the
 * first two segments of its scope, `storage.lock`), else the opened scope's
 * own name (`''` for "no scope").
 */
export function subKey(n) {
  const full = n.scope || '';
  const parts = full.split('.');
  if (parts.length >= 2 && parts[0] && parts[1]) return `${parts[0]}.${parts[1]}`;
  return n.top_scope || '';
}

/**
 * Neutral sub-scope regions for a style that does not lay out with
 * scope-force (its own backdrop expects its own layout): one path per
 * region, the union of a disc around each memory, and the layout's names.
 */
export function drawRegions(keys, lay) {
  const rr = lay.region_r || 24;
  const arcs = new Map();
  keys.forEach((key, i) => {
    if (!key) return;
    const x = lay.pos[2 * i];
    const y = lay.pos[2 * i + 1];
    const q = rr.toFixed(1);
    const d = `M${(x - rr).toFixed(1)},${y.toFixed(1)}a${q},${q} 0 1,0 ${(2 * rr).toFixed(1)},0a${q},${q} 0 1,0 ${(-2 * rr).toFixed(1)},0`;
    if (!arcs.has(key)) arcs.set(key, []);
    arcs.get(key).push(d);
  });
  const below = s('g', { class: 'sub-regions', 'aria-hidden': 'true' });
  [...arcs.keys()].sort().forEach((key, j) => below.appendChild(s('path', { class: ['sub-region', 'tone-' + (j % 3)], d: arcs.get(key).join('') })));
  const above = s('g', { class: 'sub-names', 'aria-hidden': 'true' });
  for (const nm of lay.scopes || []) {
    if (!nm.placed) continue;
    above.appendChild(s('text', { class: 'sub-name', x: nm.x.toFixed(1), y: nm.y.toFixed(1), 'font-size': nm.size.toFixed(1), 'text-anchor': 'middle' }, nm.key));
  }
  return { below, above };
}

/**
 * Where each far scope's tag goes. `bounds`: the laid-out memories;
 * `dirs`: name → [dx, dy] (any length; [0, 0] when unknown); `tags`:
 * [{ name, text }] most relations first; `boxW`/`boxH`: the graph box in
 * px. Sized so a tag reads at about 12px when the whole view is fitted.
 * Returns { tags: [{ name, x, y, w, h, font, ax, ay }] (centre, size, and
 * the point on the ring stubs aim at), bounds } — bounds covering memories
 * and tags, for Fit.
 */
export function placeTags(bounds, dirs, tags, boxW, boxH) {
  const aspect = boxH / Math.max(1, boxW);
  const cx = bounds.x + bounds.w / 2;
  const cy = bounds.y + bounds.h / 2;
  const unitsPerPx = (w, hh) => Math.max(w, hh / aspect) / Math.max(1, boxW);
  const u0 = unitsPerPx(bounds.w, bounds.h);
  const margin = 70 * u0;
  const u = unitsPerPx(bounds.w + 2 * margin, bounds.h + 2 * margin);
  const font = 11.5 * u;
  const th = 18 * u;
  const rx = bounds.w / 2 + margin * 0.55;
  const ry = bounds.h / 2 + margin * 0.55;
  const placed = [];
  const out = [];
  tags.forEach((tg, k) => {
    let [dx, dy] = dirs.get(tg.name) || [0, 0];
    if (!dx && !dy) {
      // No known direction: spread around the ring, by order.
      const a = (k / Math.max(1, tags.length)) * 2 * Math.PI - Math.PI / 2;
      dx = Math.cos(a);
      dy = Math.sin(a);
    }
    const kk = 1 / Math.sqrt((dx / rx) * (dx / rx) + (dy / ry) * (dy / ry));
    const ax = cx + dx * kk;
    const ay = cy + dy * kk;
    const w = (tg.text.length * 6.9 + 14) * u;
    // Slide along the ring's tangent until it overlaps no placed tag.
    const len = Math.sqrt(dx * dx + dy * dy) || 1;
    const tx = -dy / len;
    const ty = dx / len;
    let x = ax;
    let y = ay;
    for (let step = 0; step < 40; step++) {
      const off = (step % 2 ? 1 : -1) * Math.ceil(step / 2) * th * 1.25;
      x = ax + tx * off;
      y = ay + ty * off;
      if (!placed.some((b) => Math.abs(b.x - x) < (b.w + w) / 2 + 2 * u && Math.abs(b.y - y) < th + 2 * u)) break;
    }
    const tag = { name: tg.name, x, y, w, h: th, font, ax: x, ay: y };
    placed.push(tag);
    out.push(tag);
  });
  let x0 = bounds.x;
  let y0 = bounds.y;
  let x1 = bounds.x + bounds.w;
  let y1 = bounds.y + bounds.h;
  for (const tg of out) {
    x0 = Math.min(x0, tg.x - tg.w / 2 - 4 * u);
    x1 = Math.max(x1, tg.x + tg.w / 2 + 4 * u);
    y0 = Math.min(y0, tg.y - tg.h / 2 - 4 * u);
    y1 = Math.max(y1, tg.y + tg.h / 2 + 4 * u);
  }
  return { tags: out, bounds: { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } };
}
