// The Grid theme's layout (route-map layout v2): a seeded force simulation
// snapped to a grid, with collision-free station labels. Theme-owned, so it
// keeps its own words (stations, lines, routes); the core view only reads
// `pos`, `routes`, `labels` and `bounds`, and the Grid theme's backdrop reads
// `lines`.
//
// Pure functions over plain data, so the same module runs in the layout
// worker (graph/layout-worker.js, through graph/layouts.js) and, when a
// worker cannot start, on the main thread. Deterministic: the only
// randomness is a PRNG seeded from the node ids, and every loop walks nodes
// in id order, so the same store gives the same map on every load (and in
// every browser).
//
// Steps:
//   1. Lines (logical scopes) start as compact blocks, each line's stations
//      in a serpentine so consecutive stations are neighbours; blocks are
//      shelf-packed to the map box's aspect ratio, so Fit uses the whole box.
//   2. A force simulation: soft repulsion through a spatial hash (O(n) per
//      step, so ~1000 stations stay fast), springs along edges, stronger
//      springs along each line's station chain, weak gravity to the line's
//      block, all under a cooling step cap.
//   3. Snap: stations claim points of a grid wider than it is tall (labels
//      are wider than they are tall), most connected first, each taking the
//      nearest free point.
//   4. Lines: each line visits its stations in a nearest-neighbour order
//      from one end, so it does not double back across the map.
//   5. Routing: line chains and edges become polylines of horizontal,
//      vertical and diagonal segments (straight–diagonal–straight for lines,
//      diagonal–straight for edges, so an edge rarely hides under its line).
//      True octilinear metro routing (no crossings) is out of scope.
//   6. Labels: the core's graph/labels.js, with each line's badge as an
//      obstacle; a label with no free spot comes back `placed: false` (the
//      page draws it hidden and shows it on hover and focus).
//
// Positions depend on the nodes and edges given and nothing else, so the
// caller can keep a map steady across display filters by laying out one
// fixed edge set and only drawing a subset of it.
//
// Input:  { nodes: [{ id, top_scope, slot, scope, degree, r, label, weight }],
//           edges: [{ s, t }], aspect: box height / width }
//         (`t` < 0: a stub — dangling or cross-store — drawn off `s`).
// Output: { pos: [x0, y0, x1, y1, …], routes: [[[x, y], …], …] per edge,
//           lines: [{ key, slot, pts: [[x, y], …] }],
//           labels: [{ x, y, w, anchor, placed }] per node, bounds: {x, y, w, h} }
//         (the contract every layout meets is in graph/layouts.js).

import { placeLabels } from '../../labels.js';

export const GRID = 48;
/** Grid pitch after snapping: columns are wider than rows (labels). */
export const GX = 104;
export const GY = 60;

function fnv(str) {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/** Group stations into lines, ordered like the server's slots ("no scope" last). */
function linesOf(nodes, order) {
  const map = new Map();
  for (const i of order) {
    const key = nodes[i].top_scope || '';
    if (!map.has(key)) map.set(key, { key, slot: nodes[i].slot ?? null, members: [] });
    map.get(key).members.push(i);
  }
  const lines = [...map.values()];
  lines.sort((a, b) => (a.slot === null) - (b.slot === null) || (a.slot ?? 0) - (b.slot ?? 0) || cmp(a.key, b.key));
  // Along a line: by full scope (sub-scopes sit together), then by id.
  for (const l of lines) l.members.sort((a, b) => cmp(nodes[a].scope || '', nodes[b].scope || '') || cmp(nodes[a].id, nodes[b].id));
  return lines;
}

/** Step 1: serpentine blocks, shelf-packed. */
function seed(nodes, lines, rand, px, py, aspect) {
  const step = GRID * 1.5;
  const blocks = lines.map((l) => {
    const m = l.members.length;
    const cols = Math.max(1, Math.min(m, Math.ceil(Math.sqrt(m * 2.5))));
    const rows = Math.ceil(m / cols);
    return { l, cols, rows, w: cols * step + GRID * 2, h: rows * step + GRID * 2 };
  });
  const area = blocks.reduce((a, b) => a + b.w * b.h, 0);
  const widest = blocks.reduce((a, b) => Math.max(a, b.w), 0);
  // Pack for the box: the snap stretches x and y by different factors (GX
  // vs GY per cell), so the simulation's own aspect is the box's scaled by
  // GX/GY.
  const simAspect = Math.min(4, Math.max(0.2, (aspect || 0.6) * (GX / GY)));
  const width = Math.max(widest, Math.sqrt(area / simAspect));
  let x = 0;
  let y = 0;
  let shelf = 0;
  for (const b of blocks) {
    if (x > 0 && x + b.w > width) {
      x = 0;
      y += shelf;
      shelf = 0;
    }
    b.x = x;
    b.y = y;
    x += b.w;
    shelf = Math.max(shelf, b.h);
    b.l.members.forEach((i, k) => {
      const r = Math.floor(k / b.cols);
      const c0 = k % b.cols;
      const c = r % 2 === 0 ? c0 : b.cols - 1 - c0;
      px[i] = b.x + GRID + c * step + (rand() - 0.5) * GRID * 0.4;
      py[i] = b.y + GRID + r * step + (rand() - 0.5) * GRID * 0.4;
    });
    b.l.cx = b.x + b.w / 2;
    b.l.cy = b.y + b.h / 2;
  }
}

/** Step 2: the force simulation. */
function simulate(nodes, edges, lines, px, py) {
  const n = nodes.length;
  const iters = n <= 150 ? 260 : n <= 600 ? 160 : 90;
  const R = GRID * 1.9; // repulsion reach
  const cell = R;
  const lineOf = new Int32Array(n);
  lines.forEach((l, li) => l.members.forEach((i) => (lineOf[i] = li)));
  const dx = new Float64Array(n);
  const dy = new Float64Array(n);
  for (let it = 0; it < iters; it++) {
    const temp = GRID * (0.6 - (0.58 * it) / iters);
    dx.fill(0);
    dy.fill(0);
    // Repulsion through a spatial hash: only pairs within `R`.
    const buckets = new Map();
    for (let i = 0; i < n; i++) {
      const k = Math.floor(px[i] / cell) + ',' + Math.floor(py[i] / cell);
      let b = buckets.get(k);
      if (!b) buckets.set(k, (b = []));
      b.push(i);
    }
    for (let i = 0; i < n; i++) {
      const cx = Math.floor(px[i] / cell);
      const cy = Math.floor(py[i] / cell);
      for (let ox = -1; ox <= 1; ox++) {
        for (let oy = -1; oy <= 1; oy++) {
          const b = buckets.get(cx + ox + ',' + (cy + oy));
          if (!b) continue;
          for (const j of b) {
            if (j <= i) continue;
            let ddx = px[i] - px[j];
            let ddy = py[i] - py[j];
            let d = Math.hypot(ddx, ddy);
            if (d >= R) continue;
            if (d < 0.01) {
              // Coincident: separate along a fixed, index-derived direction.
              const a = ((i * 7 + j * 13) % 16) * (Math.PI / 8);
              ddx = Math.cos(a);
              ddy = Math.sin(a);
              d = 1;
            }
            const f = ((R - d) / R) * 0.5;
            dx[i] += (ddx / d) * f * GRID * 0.5;
            dy[i] += (ddy / d) * f * GRID * 0.5;
            dx[j] -= (ddx / d) * f * GRID * 0.5;
            dy[j] -= (ddy / d) * f * GRID * 0.5;
          }
        }
      }
    }
    const spring = (a, b, rest, k) => {
      const ddx = px[b] - px[a];
      const ddy = py[b] - py[a];
      const d = Math.hypot(ddx, ddy) || 0.01;
      const f = (d - rest) * k;
      dx[a] += (ddx / d) * f;
      dy[a] += (ddy / d) * f;
      dx[b] -= (ddx / d) * f;
      dy[b] -= (ddy / d) * f;
    };
    for (const e of edges) if (e.t >= 0 && e.t !== e.s) spring(e.s, e.t, GRID * 2.2, 0.03);
    for (const l of lines) for (let k = 1; k < l.members.length; k++) spring(l.members[k - 1], l.members[k], GRID * 1.4, 0.08);
    for (let i = 0; i < n; i++) {
      const l = lines[lineOf[i]];
      dx[i] += (l.cx - px[i]) * 0.004;
      dy[i] += (l.cy - py[i]) * 0.004;
      const m = Math.hypot(dx[i], dy[i]);
      const s = m > temp ? temp / m : 1;
      px[i] += dx[i] * s;
      py[i] += dy[i] * s;
    }
  }
}

/** Step 3: every station claims the nearest free point of the GX × GY grid. */
function snap(nodes, px, py, aspect) {
  const n = nodes.length;
  // The simulation does not keep the seed's shape (repulsion and gravity
  // round it off), so the snap restores the box's aspect: x and y get
  // separate scales whose product keeps the density, bounded so a station
  // never moves far from its neighbours' side.
  let x0 = Infinity;
  let x1 = -Infinity;
  let y0 = Infinity;
  let y1 = -Infinity;
  for (let i = 0; i < n; i++) {
    x0 = Math.min(x0, px[i]);
    x1 = Math.max(x1, px[i]);
    y0 = Math.min(y0, py[i]);
    y1 = Math.max(y1, py[i]);
  }
  const sw = Math.max(GRID, x1 - x0);
  const sh = Math.max(GRID, y1 - y0);
  const q = Math.min(3, Math.max(1 / 3, ((aspect || 0.6) * sw * GX) / (sh * GY)));
  const order = [...Array(n).keys()].sort((a, b) => (nodes[b].degree || 0) - (nodes[a].degree || 0) || cmp(nodes[a].id, nodes[b].id));
  const taken = new Set();
  // The simulation keeps stations ~1.4 GRID apart; one grid cell per
  // 1.15 GRID keeps them about one cell apart on the stretched grid.
  const k = 1 / (GRID * 1.15);
  const kx = n > 2 ? k / Math.sqrt(q) : k;
  const ky = n > 2 ? k * Math.sqrt(q) : k;
  for (const i of order) {
    const fx = px[i] * kx;
    const fy = py[i] * ky;
    const gx0 = Math.round(fx);
    const gy0 = Math.round(fy);
    let best = null;
    for (let k = 0; k < 400 && !best; k++) {
      let bd = Infinity;
      for (let ox = -k; ox <= k; ox++) {
        for (let oy = -k; oy <= k; oy++) {
          if (Math.max(Math.abs(ox), Math.abs(oy)) !== k) continue;
          const gx = gx0 + ox;
          const gy = gy0 + oy;
          if (taken.has(gx + ',' + gy)) continue;
          const d = (gx - fx) ** 2 + (gy - fy) ** 2;
          if (d < bd) {
            bd = d;
            best = [gx, gy];
          }
        }
      }
    }
    taken.add(best[0] + ',' + best[1]);
    px[i] = best[0] * GX;
    py[i] = best[1] * GY;
  }
  return taken;
}

/** Step 4: order each line's stations as a path from one end. */
function orderLines(lines, px, py) {
  for (const l of lines) {
    const m = l.members;
    if (m.length < 3) continue;
    let cx = 0;
    let cy = 0;
    for (const i of m) {
      cx += px[i];
      cy += py[i];
    }
    cx /= m.length;
    cy /= m.length;
    // Start at the station farthest from the line's centre: an end.
    let start = m[0];
    let far = -1;
    for (const i of m) {
      const d = (px[i] - cx) ** 2 + (py[i] - cy) ** 2;
      if (d > far + 1e-9) {
        far = d;
        start = i;
      }
    }
    const left = new Set(m);
    left.delete(start);
    const path = [start];
    let cur = start;
    while (left.size) {
      let best = -1;
      let bd = Infinity;
      for (const j of left) {
        const d = (px[j] - px[cur]) ** 2 + (py[j] - py[cur]) ** 2;
        if (d < bd - 1e-9 || (Math.abs(d - bd) <= 1e-9 && j < best)) {
          bd = d;
          best = j;
        }
      }
      left.delete(best);
      path.push(best);
      cur = best;
    }
    l.members = path;
  }
}

/** Step 6: greedy, collision-free label placement. */
/** Each line's badge (the Grid backdrop draws it 24 × 24, up and left of
 * the line's first station): a label obstacle. */
function badgeBoxes(lines, px, py) {
  const out = [];
  for (const l of lines || []) {
    if (!l.key || !l.members.length) continue;
    const i = l.members[0];
    out.push({ x0: px[i] - 38, y0: py[i] - 36, x1: px[i] - 10, y1: py[i] - 8 });
  }
  return out;
}

const sign = (v) => (v > 0 ? 1 : v < 0 ? -1 : 0);

/** Straight–diagonal–straight between two points (lines). */
function lineLeg(a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const ax = Math.abs(dx);
  const ay = Math.abs(dy);
  if (ax === 0 || ay === 0 || ax === ay) return [b];
  if (ax > ay) {
    const run = (ax - ay) / 2;
    return [[a[0] + sign(dx) * run, a[1]], [b[0] - sign(dx) * run, b[1]], b];
  }
  const run = (ay - ax) / 2;
  return [[a[0], a[1] + sign(dy) * run], [b[0], b[1] - sign(dy) * run], b];
}

/** Diagonal then straight (edges). */
function edgeRoute(a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const ax = Math.abs(dx);
  const ay = Math.abs(dy);
  if (ax === 0 || ay === 0 || ax === ay) return [a, b];
  const d = Math.min(ax, ay);
  return [a, [a[0] + sign(dx) * d, a[1] + sign(dy) * d], b];
}

const DIRS = [
  [1, 0], [0, -1], [-1, 0], [0, 1],
  [1, -1], [-1, -1], [-1, 1], [1, 1],
];

/** The whole layout. */
export function layout(input) {
  const nodes = input.nodes || [];
  const edges = input.edges || [];
  const n = nodes.length;
  const px = new Float64Array(n);
  const py = new Float64Array(n);
  const order = [...Array(n).keys()].sort((a, b) => cmp(nodes[a].id, nodes[b].id));
  // Seeded from the station ids alone, so the same stations start alike
  // whatever edges are drawn.
  const rand = prng(fnv(order.map((i) => nodes[i].id).join('|')));
  const lines = linesOf(nodes, order);
  seed(nodes, lines, rand, px, py, input.aspect);
  if (n > 1) simulate(nodes, edges, lines, px, py);
  const taken = snap(nodes, px, py, input.aspect);
  orderLines(lines, px, py);
  const flat = new Array(n * 2);
  for (let i = 0; i < n; i++) {
    flat[2 * i] = px[i];
    flat[2 * i + 1] = py[i];
  }
  const labels = placeLabels(nodes, flat, badgeBoxes(lines, px, py));

  const P = (i) => [px[i], py[i]];
  const routes = edges.map((e, k) => {
    if (e.t >= 0) return edgeRoute(P(e.s), P(e.t));
    // A stub: the first free neighbouring direction, in an edge-derived order.
    const gx = px[e.s] / GX;
    const gy = py[e.s] / GY;
    const start = fnv(String(k) + (nodes[e.s] ? nodes[e.s].id : '')) % 8;
    let dir = DIRS[start];
    for (let j = 0; j < 8; j++) {
      const d = DIRS[(start + j) % 8];
      if (!taken.has(gx + d[0] + ',' + (gy + d[1]))) {
        dir = d;
        break;
      }
    }
    const len = GY * 0.75;
    const norm = Math.hypot(dir[0], dir[1]);
    return [P(e.s), [px[e.s] + (dir[0] / norm) * len, py[e.s] + (dir[1] / norm) * len]];
  });
  const lineOut = lines.map((l) => {
    const pts = [];
    l.members.forEach((i, k) => {
      if (k === 0) pts.push(P(i));
      else pts.push(...lineLeg(pts[pts.length - 1], P(i)));
    });
    return { key: l.key, slot: l.slot, pts };
  });

  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  const grow = (x, y) => {
    minX = Math.min(minX, x);
    minY = Math.min(minY, y);
    maxX = Math.max(maxX, x);
    maxY = Math.max(maxY, y);
  };
  for (let i = 0; i < n; i++) {
    const r = (nodes[i].r || 6) + 6;
    grow(px[i] - r, py[i] - r);
    grow(px[i] + r, py[i] + r);
  }
  for (const r of routes) for (const p of r) grow(p[0], p[1]);
  for (const l of labels) {
    if (!l.placed) continue;
    grow(l.box.x0, l.box.y0);
    grow(l.box.x1, l.box.y1);
  }
  // Line badges sit up and left of each line's first station.
  for (const l of lineOut) if (l.key && l.pts.length) grow(l.pts[0][0] - 40, l.pts[0][1] - 36);
  if (!n) {
    minX = minY = 0;
    maxX = maxY = GRID;
  }
  const pad = 16;
  return {
    pos: flat,
    routes,
    lines: lineOut,
    labels: labels.map((l) => ({ x: l.x, y: l.y, w: l.w, anchor: l.anchor, placed: l.placed })),
    bounds: { x: minX - pad, y: minY - pad, w: maxX - minX + pad * 2, h: maxY - minY + pad * 2 },
  };
}
