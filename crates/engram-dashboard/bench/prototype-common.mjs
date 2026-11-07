// Shared, pure helpers for the prototype layouts. Deterministic: the only
// randomness is a PRNG seeded from ids, and every loop walks in index order.

export function fnv(str) {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

export function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
export const GOLDEN = Math.PI * (3 - Math.sqrt(5));

/** Display radius used by every prototype (same rule as views/graph.js). */
export const radiusOf = (nd) => 5 + Math.min(6, Math.sqrt(nd.reads || 0) * 0.55);

// ---- Barnes-Hut quadtree (repulsion in O(n log n)) ---------------------------

function buildTree(ids, px, py, mass) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const i of ids) {
    if (px[i] < x0) x0 = px[i];
    if (px[i] > x1) x1 = px[i];
    if (py[i] < y0) y0 = py[i];
    if (py[i] > y1) y1 = py[i];
  }
  const size = Math.max(x1 - x0, y1 - y0, 1) + 1;
  const root = { x: x0, y: y0, s: size, m: 0, cx: 0, cy: 0, body: -1, kids: null };
  const insert = (node, i, depth) => {
    for (;;) {
      node.cx += px[i] * mass[i];
      node.cy += py[i] * mass[i];
      node.m += mass[i];
      if (!node.kids) {
        if (node.body < 0 && node.m === mass[i]) {
          node.body = i;
          return;
        }
        if (depth > 40) return; // coincident points: just accumulate
        node.kids = [null, null, null, null];
        const b = node.body;
        node.body = -1;
        if (b >= 0) {
          // re-insert the old body one level down (without re-adding mass here)
          const h = node.s / 2;
          const q = (px[b] >= node.x + h ? 1 : 0) + (py[b] >= node.y + h ? 2 : 0);
          node.kids[q] = { x: node.x + (q & 1 ? h : 0), y: node.y + (q & 2 ? h : 0), s: h, m: mass[b], cx: px[b] * mass[b], cy: py[b] * mass[b], body: b, kids: null };
        }
      }
      const h = node.s / 2;
      const q = (px[i] >= node.x + h ? 1 : 0) + (py[i] >= node.y + h ? 2 : 0);
      if (!node.kids[q]) {
        node.kids[q] = { x: node.x + (q & 1 ? h : 0), y: node.y + (q & 2 ? h : 0), s: h, m: mass[i], cx: px[i] * mass[i], cy: py[i] * mass[i], body: i, kids: null };
        return;
      }
      node = node.kids[q];
      depth++;
    }
  };
  for (const i of ids) insert(root, i, 0);
  const fin = (nd) => {
    if (!nd) return;
    nd.cx /= nd.m;
    nd.cy /= nd.m;
    if (nd.kids) nd.kids.forEach(fin);
  };
  fin(root);
  return root;
}

/**
 * Force-directed layout (FR forces with FA2-style degree mass, Barnes-Hut
 * repulsion, linear cooling, gravity). `ids`: node indices to place; edges
 * among them as [a, b, weight]. Positions are read from and written to px/py.
 */
export function force(ids, edgeList, px, py, opt = {}) {
  const n = ids.length;
  if (n < 2) return;
  const k = opt.k || 30; // ideal edge length
  const iters = opt.iters || (n <= 100 ? 300 : n <= 400 ? 220 : 160);
  const gravity = opt.gravity ?? 0.02;
  const theta = 0.9;
  const deg = new Map();
  for (const [a, b] of edgeList) {
    deg.set(a, (deg.get(a) || 0) + 1);
    deg.set(b, (deg.get(b) || 0) + 1);
  }
  const N = px.length;
  const mass = opt.mass || new Float64Array(N).fill(1);
  if (!opt.mass) for (const i of ids) mass[i] = 1 + (deg.get(i) || 0) * 0.5;
  const fx = new Float64Array(N);
  const fy = new Float64Array(N);
  let cx = 0, cy = 0;
  for (const i of ids) { cx += px[i]; cy += py[i]; }
  cx = opt.cx ?? cx / n;
  cy = opt.cy ?? cy / n;
  const ax = opt.aspectX || 1; // gravity scale per axis (box shape)
  const ay = opt.aspectY || 1;
  const k2 = k * k;
  const cut2 = opt.cutoff ? opt.cutoff * opt.cutoff : 0;
  for (let it = 0; it < iters; it++) {
    const temp = k * 2 * (1 - it / iters) + k * 0.02;
    for (const i of ids) { fx[i] = 0; fy[i] = 0; }
    const tree = n > 60 ? buildTree(ids, px, py, mass) : null;
    for (const i of ids) {
      if (tree) {
        const stack = [tree];
        while (stack.length) {
          const nd = stack.pop();
          if (!nd || nd.m === 0) continue;
          if (nd.body === i && !nd.kids) continue;
          let dx = px[i] - nd.cx;
          let dy = py[i] - nd.cy;
          let d2 = dx * dx + dy * dy;
          if (cut2 && d2 > cut2 + nd.s * nd.s * 2) continue; // beyond reach: no far-field push
          if (nd.kids && (nd.s * nd.s) / (d2 + 1e-9) > theta * theta) {
            for (const c of nd.kids) if (c) stack.push(c);
            continue;
          }
          if (d2 < 0.01) {
            const a = ((i * 7 + (nd.body < 0 ? 3 : nd.body) * 13) % 16) * (Math.PI / 8);
            dx = Math.cos(a) * 0.1; dy = Math.sin(a) * 0.1; d2 = 0.01;
          }
          const f = (k2 * mass[i] * nd.m) / d2; // k²/d, as force/d
          fx[i] += dx * f * 0.2;
          fy[i] += dy * f * 0.2;
        }
      } else {
        for (const j of ids) {
          if (j === i) continue;
          let dx = px[i] - px[j];
          let dy = py[i] - py[j];
          let d2 = dx * dx + dy * dy;
          if (cut2 && d2 > cut2) continue;
          if (d2 < 0.01) {
            const a = ((i * 7 + j * 13) % 16) * (Math.PI / 8);
            dx = Math.cos(a) * 0.1; dy = Math.sin(a) * 0.1; d2 = 0.01;
          }
          const f = (k2 * mass[i] * mass[j]) / d2;
          fx[i] += dx * f * 0.2;
          fy[i] += dy * f * 0.2;
        }
      }
    }
    for (const [a, b, w = 1] of edgeList) {
      const dx = px[b] - px[a];
      const dy = py[b] - py[a];
      const d = Math.hypot(dx, dy) || 0.01;
      const f = Math.min(d / k, 0.25) * w; // d²/k (as force/d), capped so a step never overshoots
      fx[a] += dx * f; fy[a] += dy * f;
      fx[b] -= dx * f; fy[b] -= dy * f;
    }
    // cohesion: each member is pulled toward its group's current centroid
    for (const [members, strength] of opt.groups || []) {
      if (members.length < 2) continue;
      let gx = 0, gy = 0;
      for (const i of members) { gx += px[i]; gy += py[i]; }
      gx /= members.length; gy /= members.length;
      for (const i of members) { fx[i] += (gx - px[i]) * strength * mass[i]; fy[i] += (gy - py[i]) * strength * mass[i]; }
    }
    for (const i of ids) {
      if (opt.fixed && opt.fixed.has(i)) continue;
      fx[i] += (cx - px[i]) * gravity * mass[i] * ax;
      fy[i] += (cy - py[i]) * gravity * mass[i] * ay;
      const m = Math.hypot(fx[i], fy[i]) / mass[i];
      const s = m > temp ? temp / m : 1;
      px[i] += (fx[i] / mass[i]) * s;
      py[i] += (fy[i] / mass[i]) * s;
    }
  }
}

/** Remove node overlaps: pairs closer than r_i + r_j + gap are pushed apart. */
export function collide(ids, px, py, r, gap = 4, passes = 30, fixed = null) {
  let maxR = 0;
  for (const i of ids) maxR = Math.max(maxR, r[i]);
  const cell = maxR * 2 + gap;
  for (let p = 0; p < passes; p++) {
    let moved = 0;
    const grid = new Map();
    for (const i of ids) {
      const key = Math.floor(px[i] / cell) * 73856093 ^ Math.floor(py[i] / cell) * 19349663;
      let b = grid.get(key);
      if (!b) grid.set(key, (b = []));
      b.push(i);
    }
    for (const i of ids) {
      const gx = Math.floor(px[i] / cell);
      const gy = Math.floor(py[i] / cell);
      for (let ox = -1; ox <= 1; ox++) {
        for (let oy = -1; oy <= 1; oy++) {
          const b = grid.get(((gx + ox) * 73856093) ^ ((gy + oy) * 19349663));
          if (!b) continue;
          for (const j of b) {
            if (j <= i) continue;
            let dx = px[j] - px[i];
            let dy = py[j] - py[i];
            let d = Math.hypot(dx, dy);
            const min = r[i] + r[j] + gap;
            if (d >= min) continue;
            if (d < 1e-6) {
              const a = ((i * 7 + j * 13) % 16) * (Math.PI / 8);
              dx = Math.cos(a); dy = Math.sin(a); d = 1;
            }
            const fi = fixed && fixed.has(i), fj = fixed && fixed.has(j);
            if (fi && fj) continue;
            const push = (min - d) / (fi || fj ? 1 : 2);
            if (!fi) { px[i] -= (dx / d) * push; py[i] -= (dy / d) * push; }
            if (!fj) { px[j] += (dx / d) * push; py[j] += (dy / d) * push; }
            moved++;
          }
        }
      }
    }
    if (!moved) break;
  }
}

/** Phyllotaxis seed: a sunflower disc, in the given order. */
export function sunflower(ids, px, py, cx, cy, spacing) {
  ids.forEach((i, k) => {
    const rr = spacing * Math.sqrt(k + 0.5);
    const a = k * GOLDEN;
    px[i] = cx + rr * Math.cos(a);
    py[i] = cy + rr * Math.sin(a);
  });
}

/**
 * Pack circles with affinity: circles [{r}] and weights w(i,j). Attraction
 * by weight, anisotropic gravity for the box shape, hard collision.
 * Returns [{x, y}].
 */
export function packCircles(circles, weight, aspect = 0.6, pad = 12) {
  const m = circles.length;
  const x = new Float64Array(m);
  const y = new Float64Array(m);
  // Seed: biggest first on a sunflower, so the big ones sit central.
  const order = [...Array(m).keys()].sort((a, b) => circles[b].r - circles[a].r || a - b);
  const avg = circles.reduce((s, c) => s + c.r, 0) / Math.max(1, m);
  order.forEach((i, k) => {
    const rr = avg * 1.6 * Math.sqrt(k);
    x[i] = rr * Math.cos(k * GOLDEN) / Math.sqrt(aspect);
    y[i] = rr * Math.sin(k * GOLDEN) * Math.sqrt(aspect);
  });
  const gx = aspect; // landscape box (h/w < 1): weaker x gravity -> wider
  const gy = 1;
  for (let it = 0; it < 400; it++) {
    const t = 1 - it / 400;
    for (let i = 0; i < m; i++) {
      for (let j = i + 1; j < m; j++) {
        const w = weight(i, j);
        if (!w) continue;
        const dx = x[j] - x[i];
        const dy = y[j] - y[i];
        const d = Math.hypot(dx, dy) || 1;
        const gapd = d - (circles[i].r + circles[j].r + pad);
        const f = Math.min(1, w * 0.02) * gapd * 0.1 * t;
        x[i] += (dx / d) * f; y[i] += (dy / d) * f;
        x[j] -= (dx / d) * f; y[j] -= (dy / d) * f;
      }
    }
    for (let i = 0; i < m; i++) {
      x[i] -= x[i] * 0.02 * gx;
      y[i] -= y[i] * 0.02 * gy;
    }
    for (let p = 0; p < 4; p++) {
      for (let i = 0; i < m; i++) {
        for (let j = i + 1; j < m; j++) {
          let dx = x[j] - x[i];
          let dy = y[j] - y[i];
          let d = Math.hypot(dx, dy);
          const min = circles[i].r + circles[j].r + pad;
          if (d >= min) continue;
          if (d < 1e-6) { dx = Math.cos(i + j); dy = Math.sin(i + j); d = 1; }
          const push = (min - d) / 2;
          // heavier circles move less
          const wi = circles[j].r / (circles[i].r + circles[j].r);
          x[i] -= (dx / d) * push * 2 * wi; y[i] -= (dy / d) * push * 2 * wi;
          x[j] += (dx / d) * push * 2 * (1 - wi); y[j] += (dy / d) * push * 2 * (1 - wi);
        }
      }
    }
  }
  return [...Array(m).keys()].map((i) => ({ x: x[i], y: y[i] }));
}

/** Shelf-pack rectangles [{w, h}] into rows about `width` wide. Returns [{x, y}]. */
export function shelf(rects, width, gap = 20) {
  let x = 0, y = 0, row = 0;
  return rects.map((r) => {
    if (x > 0 && x + r.w > width) { x = 0; y += row + gap; row = 0; }
    const out = { x, y };
    x += r.w + gap;
    row = Math.max(row, r.h);
    return out;
  });
}

export function bbox(ids, px, py, r) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const i of ids) {
    const rr = r ? r[i] : 0;
    x0 = Math.min(x0, px[i] - rr); x1 = Math.max(x1, px[i] + rr);
    y0 = Math.min(y0, py[i] - rr); y1 = Math.max(y1, py[i] + rr);
  }
  if (!isFinite(x0)) return { x0: 0, y0: 0, x1: 0, y1: 0, w: 0, h: 0 };
  return { x0, y0, x1, y1, w: x1 - x0, h: y1 - y0 };
}

/**
 * Park memories with no relations in a compact grid per scope, in a band
 * below the main graph (`main` bbox). Returns group boxes [{x,y,w,h,label}].
 */
export function parkGrid(isoIds, nodes, px, py, main, pitch = 22, labelH = 16) {
  const by = new Map();
  for (const i of isoIds) {
    const key = nodes[i].scope || '';
    if (!by.has(key)) by.set(key, []);
    by.get(key).push(i);
  }
  const keys = [...by.keys()].sort((a, b) => (a === '') - (b === '') || cmp(a, b));
  const width = Math.max(main.w, pitch * 12);
  const blocks = keys.map((key) => {
    const ids = by.get(key).sort((a, b) => cmp(nodes[a].kind, nodes[b].kind) || cmp(nodes[a].id, nodes[b].id));
    const cols = Math.max(1, Math.min(ids.length, Math.ceil(Math.sqrt(ids.length * 2))));
    const rows = Math.ceil(ids.length / cols);
    return { key, ids, cols, w: Math.max(cols * pitch, key.length * 6.5 + 8), h: rows * pitch + labelH };
  });
  const at = shelf(blocks, width, pitch * 1.2);
  const top = main.y1 + pitch * 3;
  const groups = [];
  blocks.forEach((b, k) => {
    const ox = main.x0 + at[k].x;
    const oy = top + at[k].y;
    b.ids.forEach((i, q) => {
      px[i] = ox + (q % b.cols) * pitch + pitch / 2;
      py[i] = oy + labelH + Math.floor(q / b.cols) * pitch + pitch / 2;
    });
    groups.push({ kind: 'box', x: ox, y: oy, w: b.w, h: b.h, label: b.key || '(no scope)' });
  });
  return groups;
}

/**
 * Greedy spiral packing: circles [{r}] in the given order, each at the first
 * point of an (aspect-shaped) spiral from the centre where it overlaps none
 * already placed. Fast for hundreds of circles. Returns [{x, y}].
 */
export function spiralPack(circles, aspect = 0.62, pad = 8) {
  const out = [];
  const sx = 1 / Math.sqrt(aspect);
  const sy = Math.sqrt(aspect);
  const CELL = 40;
  const grid = new Map();
  const cells = (x, y, r, f) => {
    for (let gx = Math.floor((x - r) / CELL); gx <= Math.floor((x + r) / CELL); gx++)
      for (let gy = Math.floor((y - r) / CELL); gy <= Math.floor((y + r) / CELL); gy++) f(gx + ',' + gy);
  };
  const STEP = 5;
  let kPrev = 0;
  for (const c of circles) {
    let found = null;
    let k = Math.floor(kPrev * 0.6);
    for (; !found; k++) {
      const rr = STEP * Math.sqrt(k);
      const x = rr * Math.cos(k * GOLDEN) * sx;
      const y = rr * Math.sin(k * GOLDEN) * sy;
      let ok = true;
      cells(x, y, c.r + pad, (key) => {
        if (!ok) return;
        for (const p of grid.get(key) || []) if (Math.hypot(p.x - x, p.y - y) < p.r + c.r + pad) { ok = false; return; }
      });
      if (ok) found = { x, y };
    }
    kPrev = k;
    out.push(found);
    const rec = { ...found, r: c.r };
    cells(found.x, found.y, c.r, (key) => {
      if (!grid.has(key)) grid.set(key, []);
      grid.get(key).push(rec);
    });
  }
  return out;
}
