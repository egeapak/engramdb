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
 * `dirs`: name → [dx, dy] (any length; absent when unknown); `tags`:
 * [{ name, text }] most relations first; `boxW`/`boxH`: the graph box in
 * px. Sized so a tag reads at about 12px when the whole view is fitted.
 *
 * Each tag starts on a ring around the memories at its scope's direction;
 * tags that share a direction are then pushed apart *around* the ring
 * (never stacked in one column), a gap between neighbours, until none
 * overlaps; a far scope with no known direction takes an even share of the
 * ring. Returns { tags: [{ name, x, y, w, h, font, ax, ay }] (centre, size,
 * and the point stubs aim at), bounds } — bounds covering memories and
 * tags, for Fit.
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
  const gap = 8 * u;
  const rx = bounds.w / 2 + margin * 0.55;
  const ry = bounds.h / 2 + margin * 0.55;
  const at = (a) => [cx + rx * Math.cos(a), cy + ry * Math.sin(a)];
  const items = tags.map((tg, k) => {
    const d = dirs.get(tg.name);
    // Angles on the ellipse, so a direction lands where it points.
    const a = d && (d[0] || d[1]) ? Math.atan2(d[1] / ry, d[0] / rx) : (k / Math.max(1, tags.length)) * 2 * Math.PI - Math.PI / 2;
    return { name: tg.name, a, w: (tg.text.length * 6.9 + 14) * u };
  });
  const overlap = (p, q) => {
    const [px, py] = at(p.a);
    const [qx, qy] = at(q.a);
    return Math.abs(px - qx) < (p.w + q.w) / 2 + gap && Math.abs(py - qy) < th + gap;
  };
  // Push overlapping neighbours apart around the ring (both move, so a
  // cluster spreads about its own centre); bounded passes.
  const step = 0.035;
  for (let pass = 0; pass < 400; pass++) {
    items.sort((p, q) => p.a - q.a);
    let moved = false;
    for (let i = 0; i < items.length; i++) {
      for (let j = i + 1; j < items.length; j++) {
        if (!overlap(items[i], items[j])) continue;
        const lo = items[i];
        const hi = items[j];
        // The shorter way round decides which one is "ahead".
        let diff = hi.a - lo.a;
        if (diff > Math.PI) diff -= 2 * Math.PI;
        const sgn = diff >= 0 ? 1 : -1;
        lo.a -= sgn * step;
        hi.a += sgn * step;
        moved = true;
      }
    }
    if (!moved) break;
  }
  const out = items.map((it) => {
    const [x, y] = at(it.a);
    return { name: it.name, x, y, w: it.w, h: th, font, ax: x, ay: y };
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
