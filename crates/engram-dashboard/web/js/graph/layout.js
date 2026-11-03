// Route-map layout v1: a seeded force simulation snapped to a grid.
//
// Pure functions over plain data, so the same module runs in the layout
// worker (layout-worker.js) and, when a worker cannot start, on the main
// thread. Deterministic: the only randomness is a PRNG seeded from the node
// ids, and every loop walks nodes in id order, so the same store gives the
// same map on every load (and in every browser).
//
// Steps:
//   1. Lines (logical scopes) start as compact blocks, each line's stations
//      in a serpentine so consecutive stations are neighbours; blocks are
//      shelf-packed.
//   2. A force simulation: soft repulsion through a spatial hash (O(n) per
//      step, so ~1000 stations stay fast), springs along edges, stronger
//      springs along each line's station chain, weak gravity to the line's
//      block, all under a cooling step cap.
//   3. Snap: stations claim grid points, most connected first, each taking
//      the nearest free point.
//   4. Routing: line chains and edges become polylines of horizontal,
//      vertical and 45° segments (straight–diagonal–straight for lines,
//      diagonal–straight for edges, so an edge rarely hides under its line).
//      True octilinear metro routing (no crossings, aligned stations) is
//      out of scope for v1.
//
// Input:  { nodes: [{ id, line, slot, scope, degree }], edges: [{ s, t }] }
//         (`t` < 0: a stub — dangling or cross-store — drawn off `s`).
// Output: { pos: [x0, y0, x1, y1, …], routes: [[[x, y], …], …] per edge,
//           lines: [{ key, slot, pts: [[x, y], …] }], bounds: {x, y, w, h} }

export const GRID = 48;

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
    const key = nodes[i].line || '';
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
function seed(nodes, lines, rand, px, py) {
  const step = GRID * 1.5;
  const blocks = lines.map((l) => {
    const m = l.members.length;
    const cols = Math.max(1, Math.min(m, Math.ceil(Math.sqrt(m * 2.5))));
    const rows = Math.ceil(m / cols);
    return { l, cols, rows, w: cols * step + GRID * 2, h: rows * step + GRID * 2 };
  });
  const area = blocks.reduce((a, b) => a + b.w * b.h, 0);
  const widest = blocks.reduce((a, b) => Math.max(a, b.w), 0);
  const width = Math.max(widest, Math.sqrt(area) * 1.4);
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

/** Step 3: every station claims the nearest free grid point. */
function snap(nodes, px, py) {
  const n = nodes.length;
  const order = [...Array(n).keys()].sort((a, b) => (nodes[b].degree || 0) - (nodes[a].degree || 0) || cmp(nodes[a].id, nodes[b].id));
  const taken = new Set();
  for (const i of order) {
    const fx = px[i] / GRID;
    const fy = py[i] / GRID;
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
    px[i] = best[0] * GRID;
    py[i] = best[1] * GRID;
  }
  return taken;
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
  const rand = prng(fnv(order.map((i) => nodes[i].id).join('|') + '#' + edges.length));
  const lines = linesOf(nodes, order);
  seed(nodes, lines, rand, px, py);
  if (n > 1) simulate(nodes, edges, lines, px, py);
  const taken = snap(nodes, px, py);

  const P = (i) => [px[i], py[i]];
  const routes = edges.map((e, k) => {
    if (e.t >= 0) return edgeRoute(P(e.s), P(e.t));
    // A stub: the first free neighbouring direction, in an edge-derived order.
    const gx = px[e.s] / GRID;
    const gy = py[e.s] / GRID;
    const start = fnv(String(k) + (nodes[e.s] ? nodes[e.s].id : '')) % 8;
    let dir = DIRS[start];
    for (let j = 0; j < 8; j++) {
      const d = DIRS[(start + j) % 8];
      if (!taken.has(gx + d[0] + ',' + (gy + d[1]))) {
        dir = d;
        break;
      }
    }
    const len = GRID * 0.9;
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
  for (let i = 0; i < n; i++) grow(px[i], py[i]);
  for (const r of routes) for (const p of r) grow(p[0], p[1]);
  if (!n) {
    minX = minY = 0;
    maxX = maxY = GRID;
  }
  const pad = GRID * 1.5;
  return {
    pos: Array.from({ length: n * 2 }, (_, k) => (k % 2 ? py[k >> 1] : px[k >> 1])),
    routes,
    lines: lineOut,
    bounds: { x: minX - pad, y: minY - pad, w: maxX - minX + pad * 2 + GRID * 4, h: maxY - minY + pad * 2 },
  };
}
