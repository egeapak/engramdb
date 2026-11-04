// Scope-aware force layout: memories placed by their relations, gathered by
// scope. The Plain style's layout, and core rather than Plain's own because
// nothing in it is a look (it returns positions, straight relations and
// where each scope's name goes), so the opened-scope view of phase E and any
// later style can lay a graph out the same way without importing a style.
//
// Steps:
//   1. Start: every scope gets a disc sized by its memories. Sub-scopes are
//      packed inside their top scope's disc, top scopes are packed next to
//      the scopes they share the most relations with (scope affinity), and
//      each sub-scope's disc is turned to face the top scopes its memories
//      relate to. Memories start on a sunflower in their own (sub-)scope's
//      disc, most connected in the middle. A start this close to the end
//      is what lets the simulation run few steps.
//   2. Simulation: repulsion cut off at CUT (6 spacings, tapered to zero, so
//      nothing is pushed away from the graph), exact within two spacings and
//      by grid-cell centres of mass beyond, through a spatial grid (counting
//      sort, typed arrays: O(n) per step); springs along relations; a pull
//      toward the memory's top scope's and sub-scope's current centres;
//      weak gravity, stronger in y for a box wider than tall; every step
//      capped by a cooling temperature.
//   3. Spread: a small graph is scaled up so its labels have room (a large
//      one could never show every label at Fit anyway); then overlaps are
//      pushed apart.
//   4. Scope names: one per top scope, near its centre on a spot over no
//      memory and no other scope's name, sized for the whole graph.
//   5. Memory labels: graph/labels.js, with the scope names as obstacles;
//      a memory of a small graph whose label has no spot may be nudged
//      (at most 1.5 spacings, never toward another scope's centre, never
//      adding a crossing, on a bounded budget). Then the names are placed
//      again at the size they are drawn, which depends on the box's width
//      (readable at Fit on a phone) — the only output that does.
//   6. Relations are straight two-point routes. A stub (the far end is not
//      drawn: deleted, or another store) is a short segment toward its far
//      scope when that scope is on the graph, otherwise away from the
//      memory's neighbours; several stubs on one memory fan out.
//
// Deterministic: no random numbers at all (every start position comes from
// the sorted ids and the scope sizes), every loop walks memories in id order
// and relations in endpoint order, and positions use only + − × ÷ and
// Math.sqrt, which are exact-rounded in every engine (Math.cos / sin / hypot
// / pow are implementation-approximated and could round differently in
// another browser; the few angles needed are literal constants). Positions
// depend only on the nodes, the edges and the box's aspect, never on its
// width (only the drawn scope names do, step 5), so a store draws the same
// graph on every screen of one shape, and the caller can keep a graph
// steady across display filters by laying out one fixed edge set.
//
// Pure functions over plain data: it runs in the layout worker and, when a
// worker cannot start, on the main thread.
//
// Input / output: the contract in graph/layouts.js. Extra output, read by
// the style's backdrop: `scopes: [{ key, slot, x, y, w, size, placed,
// backed }]` (one name per named top scope, `size` its font size and `w` its
// estimated width in layout units, `placed` false when no free spot was
// found, so the style may skip it, `backed` when it covers memories and
// wants a faint backing box) and `region_r` (a radius around each memory,
// at least its own r + 6, that unioned per scope draws a soft region; the
// bounds already cover it).

import { placeLabels } from './labels.js';

/** The ideal relation length, in layout units (a node's radius is 5–11). */
const S = 30;
/** Repulsion reach: six spacings, beyond which no memory pushes another. */
const CUT = 6 * S;
/** Repulsion grid cell: exact pairs within one cell, centres of mass out to three (3 cells ≥ CUT). */
const CELL = 2 * S;
/** Sunflower spacing at the start: about one spacing between neighbours. */
const SEED = 0.56 * S;
/** The golden angle's cosine and sine, as literals (see "Deterministic" above). */
const GA_C = -0.7373688780783197;
const GA_S = 0.6754902942615238;
const R3 = 0.8660254037844386; // √3 / 2
/** Twelve unit directions, 30° apart, counter-clockwise from +x. */
const DIRS = [
  [1, 0], [R3, 0.5], [0.5, R3], [0, 1], [-0.5, R3], [-R3, 0.5],
  [-1, 0], [-R3, -0.5], [-0.5, -R3], [0, -1], [0.5, -R3], [R3, -0.5],
];
/** Above this many scopes to pack, a sunflower by size replaces the affinity packing (O(m²) per scope). */
const PACK_MAX = 100;
/** At most this many (group, other top) pairs weigh a top scope's turn. */
const TURN_MAX = 96;
/** The width a small graph is spread toward (see step 3). */
const SPREAD_TO = 860;
/** Up to this many memories, a label with no free spot may move its memory (step 5). */
const NUDGE_MAX = 300;
/** A memory with more relations than this is never nudged (see nudgeTest). */
const NUDGE_DEGREE = 24;
/** The crossing tests all nudges of one layout may spend (see nudgeTest). */
const NUDGE_WORK = 300000;
/** Cell-index cap per axis for the repulsion grid (positions are bounded; this only guards the allocation). */
const GRID_MAX = 1024;

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/** Options a style may pass (`layout.options`); all optional. */
const DEFAULTS = Object.freeze({
  topPull: 0.025, // toward the top scope's centre, per unit distance and mass
  subPull: 0.02, // toward the sub-scope's centre
  gravity: 0.008,
  repulsion: 0.2,
  steps: 1, // a factor on the step count
  temp: 0.9, // the first step's cap, in spacings
});

// ---------------------------------------------------------------------------
// Start positions
// ---------------------------------------------------------------------------

/**
 * Pack `m` discs (radii `rad`, in priority order: the first is placed first)
 * with affinity `aff` (m × m, row-major; may be null): each next disc is the
 * unplaced one most related to those already placed, put touching a placed
 * one where its related discs are nearest and the pack stays compact (a
 * distance stretched by the box shape). Returns centres [x0, y0, …].
 */
function pack(rad, aff, aspect, pad) {
  const m = rad.length;
  const out = new Float64Array(m * 2);
  if (m <= 1) return out;
  const gx = aspect; // a box wider than tall: moving in x is cheaper
  const gy = 1 / aspect;
  if (m > PACK_MAX) {
    // Too many to compare pairwise: a sunflower by priority, wide for the box.
    let c = 1;
    let sn = 0;
    let avg = 0;
    for (let j = 0; j < m; j++) avg += rad[j];
    avg /= m;
    for (let j = 0; j < m; j++) {
      const rr = avg * 2.1 * Math.sqrt(j);
      out[2 * j] = (rr * c) / Math.sqrt(aspect);
      out[2 * j + 1] = rr * sn * Math.sqrt(aspect);
      const nc = c * GA_C - sn * GA_S;
      sn = c * GA_S + sn * GA_C;
      c = nc;
    }
    return out;
  }
  const placed = new Uint8Array(m);
  const toPlaced = new Float64Array(m); // affinity of each disc to the placed ones
  const order = [0];
  placed[0] = 1;
  if (aff) for (let j = 0; j < m; j++) toPlaced[j] += aff[j * m];
  for (let step = 1; step < m; step++) {
    let j = -1;
    for (let q = 0; q < m; q++) if (!placed[q] && (j < 0 || toPlaced[q] > toPlaced[j])) j = q;
    let total = 0;
    if (aff) for (const q of order) total += aff[j * m + q];
    let best = Infinity;
    let bx = 0;
    let by = 0;
    // Anchor on every placed disc while few; then on the last 24 and the 12
    // most related (all related would make a densely related pack cubic).
    let anchors = order;
    if (order.length > 36) {
      anchors = order.slice(order.length - 24);
      if (aff) {
        const rel = order
          .slice(0, order.length - 24)
          .filter((q) => aff[j * m + q] > 0)
          .sort((p, q) => aff[j * m + q] - aff[j * m + p] || p - q);
        anchors = anchors.concat(rel.slice(0, 12));
      }
    }
    for (const p of anchors) {
      const reach = rad[p] + rad[j] + pad;
      for (const [dx, dy] of DIRS) {
        const cx = out[2 * p] + dx * reach;
        const cy = out[2 * p + 1] + dy * reach;
        let ok = true;
        let cost = 0;
        for (const q of order) {
          const ex = cx - out[2 * q];
          const ey = cy - out[2 * q + 1];
          const d2 = ex * ex + ey * ey;
          const min = rad[q] + rad[j] + pad - 1e-6;
          if (d2 < min * min) {
            ok = false;
            break;
          }
          if (total > 0) {
            const w = aff[j * m + q];
            if (w) cost += (w / total) * Math.sqrt(d2);
          }
        }
        if (!ok) continue;
        cost += Math.sqrt(cx * cx * gx + cy * cy * gy);
        if (cost < best - 1e-9) {
          best = cost;
          bx = cx;
          by = cy;
        }
      }
    }
    out[2 * j] = bx;
    out[2 * j + 1] = by;
    placed[j] = 1;
    order.push(j);
    if (aff) for (let q = 0; q < m; q++) toPlaced[q] += aff[q * m + j];
  }
  return out;
}

/** Sunflower: `ids` (in order) around (cx, cy), the first in the middle. */
function sunflower(ids, x, y, cx, cy, sp) {
  let c = 1;
  let sn = 0;
  for (let k = 0; k < ids.length; k++) {
    const rr = sp * Math.sqrt(k + 0.5);
    x[ids[k]] = cx + rr * c;
    y[ids[k]] = cy + rr * sn;
    const nc = c * GA_C - sn * GA_S;
    sn = c * GA_S + sn * GA_C;
    c = nc;
  }
}

// ---------------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------------

/**
 * The force simulation over positions `x`, `y` (internal index = id order).
 * `ea`/`eb`: relation endpoints; `topOf`/`subOf`: group index per memory
 * (-1: none) with `nTop`/`nSub` groups. Each step is a few small functions
 * over one state object rather than one long loop body, so the engine
 * compiles the hot parts after a few steps instead of interpreting a
 * single huge function for most of a cold run.
 */
function simulate(n, x, y, ea, eb, mass, topOf, nTop, topW, subOf, nSub, aspect, iters, opt) {
  const st = {
    n, x, y, ea, eb, mass, topOf, topW, subOf, nTop, nSub,
    fx: new Float64Array(n),
    fy: new Float64Array(n),
    tx: new Float64Array(nTop),
    ty: new Float64Array(nTop),
    tm: new Float64Array(nTop),
    sx: new Float64Array(nSub),
    sy: new Float64Array(nSub),
    sm: new Float64Array(nSub),
    cellOf: new Int32Array(n),
    items: new Int32Array(n),
    start: new Int32Array(1),
    fill: new Int32Array(1),
    cmass: new Float64Array(0),
    cmx: new Float64Array(0),
    cmy: new Float64Array(0),
    farX: new Float64Array(0),
    farY: new Float64Array(0),
    gw: 1,
    gh: 1,
    rep: opt.repulsion,
    topPull: opt.topPull,
    subPull: opt.subPull,
    ax: opt.gravity * aspect,
    ay: opt.gravity / aspect,
  };
  const t0 = S * opt.temp;
  for (let it = 0; it < iters; it++) {
    st.fx.fill(0);
    st.fy.fill(0);
    bin(st);
    farField(st);
    nearField(st);
    springs(st);
    centres(st);
    step(st, t0 * (1 - it / iters) + S * 0.02);
  }
}

/** The grid: a counting sort of memories into CELL-sized cells, with each cell's centre of mass. */
function bin(st) {
  const { n, x, y, mass, cellOf } = st;
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (let i = 0; i < n; i++) {
    if (x[i] < x0) x0 = x[i];
    if (x[i] > x1) x1 = x[i];
    if (y[i] < y0) y0 = y[i];
    if (y[i] > y1) y1 = y[i];
  }
  const gw = Math.min(GRID_MAX, Math.floor((x1 - x0) / CELL) + 1);
  const gh = Math.min(GRID_MAX, Math.floor((y1 - y0) / CELL) + 1);
  const cells = gw * gh;
  st.gw = gw;
  st.gh = gh;
  if (st.start.length < cells + 1) {
    st.start = new Int32Array(cells + 1);
    st.fill = new Int32Array(cells + 1);
    st.cmass = new Float64Array(cells);
    st.cmx = new Float64Array(cells);
    st.cmy = new Float64Array(cells);
    st.farX = new Float64Array(cells);
    st.farY = new Float64Array(cells);
  } else {
    st.start.fill(0, 0, cells + 1);
    st.cmass.fill(0, 0, cells);
    st.cmx.fill(0, 0, cells);
    st.cmy.fill(0, 0, cells);
  }
  const { start, fill, cmass, cmx, cmy, items } = st;
  for (let i = 0; i < n; i++) {
    const cx = Math.min(gw - 1, Math.floor((x[i] - x0) / CELL));
    const cy = Math.min(gh - 1, Math.floor((y[i] - y0) / CELL));
    const c = cy * gw + cx;
    cellOf[i] = c;
    start[c + 1]++;
    cmass[c] += mass[i];
    cmx[c] += x[i] * mass[i];
    cmy[c] += y[i] * mass[i];
  }
  for (let c = 0; c < cells; c++) {
    start[c + 1] += start[c];
    fill[c] = start[c];
    if (cmass[c] > 0) {
      cmx[c] /= cmass[c];
      cmy[c] /= cmass[c];
    }
  }
  for (let i = 0; i < n; i++) items[fill[cellOf[i]]++] = i;
}

/**
 * Repulsion (k²/d, tapered to zero at CUT) beyond the 3 × 3 cells around a
 * memory: the far cells' push (at their centres of mass) on its own cell's
 * centre of mass, computed once per occupied cell and shared by its
 * members. At two cells and more the difference from a per-memory sum is
 * small, and it makes the far field O(cells) instead of O(n).
 */
function farField(st) {
  const { gw, gh, cmass, cmx, cmy, farX, farY } = st;
  const cells = gw * gh;
  const k2 = S * S;
  const cut2 = CUT * CUT;
  const tooNear = (CELL * CELL) / 4;
  for (let c = 0; c < cells; c++) {
    farX[c] = 0;
    farY[c] = 0;
    if (!cmass[c]) continue;
    const ci = c % gw;
    const cj = (c - ci) / gw;
    const xc = cmx[c];
    const yc = cmy[c];
    let ux = 0;
    let uy = 0;
    const r0 = cj > 2 ? cj - 3 : 0;
    const r1 = cj < gh - 3 ? cj + 3 : gh - 1;
    const c0 = ci > 2 ? ci - 3 : 0;
    const c1 = ci < gw - 3 ? ci + 3 : gw - 1;
    for (let rj = r0; rj <= r1; rj++) {
      const near = rj >= cj - 1 && rj <= cj + 1;
      for (let ri = c0; ri <= c1; ri++) {
        if (near && ri >= ci - 1 && ri <= ci + 1) continue;
        const cc = rj * gw + ri;
        const m = cmass[cc];
        if (!m) continue;
        const dx = xc - cmx[cc];
        const dy = yc - cmy[cc];
        const d2 = dx * dx + dy * dy;
        if (d2 >= cut2 || d2 < tooNear) continue;
        const tp = 1 - Math.sqrt(d2) / CUT;
        const f = (k2 * m * tp * tp) / d2;
        ux += dx * f;
        uy += dy * f;
      }
    }
    farX[c] = ux;
    farY[c] = uy;
  }
}

/** Repulsion between memories within 3 × 3 cells of each other, pair by pair, plus the cell's far field. */
function nearField(st) {
  const { n, x, y, mass, cellOf, items, start, gw, gh, farX, farY, fx, fy, rep } = st;
  const k2 = S * S;
  for (let i = 0; i < n; i++) {
    const c = cellOf[i];
    const ci = c % gw;
    const cj = (c - ci) / gw;
    const xi = x[i];
    const yi = y[i];
    let ux = farX[c];
    let uy = farY[c];
    const r0 = cj > 0 ? cj - 1 : 0;
    const r1 = cj < gh - 1 ? cj + 1 : gh - 1;
    const c0 = ci > 0 ? ci - 1 : 0;
    const c1 = ci < gw - 1 ? ci + 1 : gw - 1;
    for (let rj = r0; rj <= r1; rj++) {
      // A row of up to three adjacent cells is one contiguous run of `items`.
      const end = start[rj * gw + c1 + 1];
      for (let q = start[rj * gw + c0]; q < end; q++) {
        const j = items[q];
        if (j === i) continue;
        let dx = xi - x[j];
        let dy = yi - y[j];
        let d2 = dx * dx + dy * dy;
        if (d2 < 0.01) {
          // Coincident: apart along a fixed direction from the pair's indices.
          const d = DIRS[(i * 7 + j * 5) % 12];
          dx = d[0] * (i < j ? -0.1 : 0.1);
          dy = d[1] * (i < j ? -0.1 : 0.1);
          d2 = 0.01;
        }
        // Within 3 × 3 cells a pair is under 2·√2 cells (< CUT) apart: no cutoff test.
        const tp = 1 - Math.sqrt(d2) / CUT;
        const f = (k2 * mass[j] * tp * tp) / d2;
        ux += dx * f;
        uy += dy * f;
      }
    }
    fx[i] += ux * mass[i] * rep;
    fy[i] += uy * mass[i] * rep;
  }
}

/** Relation springs: d²/k, capped so a step never overshoots. */
function springs(st) {
  const { x, y, ea, eb, fx, fy } = st;
  for (let e = 0; e < ea.length; e++) {
    const a = ea[e];
    const b = eb[e];
    const dx = x[b] - x[a];
    const dy = y[b] - y[a];
    const d = Math.sqrt(dx * dx + dy * dy) || 0.01;
    const f = d / S < 0.25 ? d / S : 0.25;
    fx[a] += dx * f;
    fy[a] += dy * f;
    fx[b] -= dx * f;
    fy[b] -= dy * f;
  }
}

/** Each top scope's and sub-scope's current centre (mean position). */
function centres(st) {
  const { n, x, y, topOf, subOf, tx, ty, tm, sx, sy, sm, nTop, nSub } = st;
  tx.fill(0);
  ty.fill(0);
  tm.fill(0);
  sx.fill(0);
  sy.fill(0);
  sm.fill(0);
  for (let i = 0; i < n; i++) {
    const t = topOf[i];
    if (t >= 0) {
      tx[t] += x[i];
      ty[t] += y[i];
      tm[t]++;
    }
    const u = subOf[i];
    if (u >= 0) {
      sx[u] += x[i];
      sy[u] += y[i];
      sm[u]++;
    }
  }
  for (let t = 0; t < nTop; t++) {
    if (tm[t]) {
      tx[t] /= tm[t];
      ty[t] /= tm[t];
    }
  }
  for (let u = 0; u < nSub; u++) {
    if (sm[u]) {
      sx[u] /= sm[u];
      sy[u] /= sm[u];
    }
  }
}

/** The pull toward the scope centres, gravity, and the step, capped at `temp`. */
function step(st, temp) {
  const { n, x, y, mass, topOf, topW, subOf, tx, ty, tm, sx, sy, sm, fx, fy, topPull, subPull, ax, ay } = st;
  const temp2 = temp * temp;
  for (let i = 0; i < n; i++) {
    const t = topOf[i];
    let gx = fx[i];
    let gy = fy[i];
    if (t >= 0 && tm[t] > 1) {
      const w = topPull * topW[t] * mass[i];
      gx += (tx[t] - x[i]) * w;
      gy += (ty[t] - y[i]) * w;
    }
    const u = subOf[i];
    if (u >= 0 && sm[u] > 1) {
      const w = subPull * topW[t] * mass[i];
      gx += (sx[u] - x[i]) * w;
      gy += (sy[u] - y[i]) * w;
    }
    gx -= x[i] * ax * mass[i];
    gy -= y[i] * ay * mass[i];
    const mx = gx / mass[i];
    const my = gy / mass[i];
    const step2 = mx * mx + my * my;
    const k = step2 > temp2 ? temp / Math.sqrt(step2) : 1;
    x[i] += mx * k;
    y[i] += my * k;
  }
}

/** Push apart memories closer than their radii plus `gap` (grid-bucketed, a few passes). */
function separate(n, x, y, r, gap) {
  if (!n) return;
  let maxR = 0;
  for (let i = 0; i < n; i++) if (r[i] > maxR) maxR = r[i];
  const cell = maxR * 2 + gap;
  const cellOf = new Int32Array(n);
  const items = new Int32Array(n);
  for (let pass = 0; pass < 30; pass++) {
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (let i = 0; i < n; i++) {
      if (x[i] < x0) x0 = x[i];
      if (x[i] > x1) x1 = x[i];
      if (y[i] < y0) y0 = y[i];
      if (y[i] > y1) y1 = y[i];
    }
    const gw = Math.min(GRID_MAX * 4, Math.floor((x1 - x0) / cell) + 1);
    const gh = Math.min(GRID_MAX * 4, Math.floor((y1 - y0) / cell) + 1);
    const start = new Int32Array(gw * gh + 1);
    for (let i = 0; i < n; i++) {
      const c = Math.min(gh - 1, Math.floor((y[i] - y0) / cell)) * gw + Math.min(gw - 1, Math.floor((x[i] - x0) / cell));
      cellOf[i] = c;
      start[c + 1]++;
    }
    for (let c = 0; c < gw * gh; c++) start[c + 1] += start[c];
    const fill = start.slice(0, gw * gh);
    for (let i = 0; i < n; i++) items[fill[cellOf[i]]++] = i;
    let moved = 0;
    for (let i = 0; i < n; i++) {
      const ci = cellOf[i] % gw;
      const cj = (cellOf[i] - ci) / gw;
      for (let oy = -1; oy <= 1; oy++) {
        const rj = cj + oy;
        if (rj < 0 || rj >= gh) continue;
        for (let ox = -1; ox <= 1; ox++) {
          const ri = ci + ox;
          if (ri < 0 || ri >= gw) continue;
          const cc = rj * gw + ri;
          for (let q = start[cc]; q < start[cc + 1]; q++) {
            const j = items[q];
            if (j <= i) continue;
            let dx = x[j] - x[i];
            let dy = y[j] - y[i];
            let d = Math.sqrt(dx * dx + dy * dy);
            const min = r[i] + r[j] + gap;
            if (d >= min) continue;
            if (d < 1e-6) {
              [dx, dy] = DIRS[(i * 7 + j * 5) % 12];
              d = 1;
            }
            const push = (min - d) / 2;
            x[i] -= (dx / d) * push;
            y[i] -= (dy / d) * push;
            x[j] += (dx / d) * push;
            y[j] += (dy / d) * push;
            moved++;
          }
        }
      }
    }
    if (!moved) break;
  }
}

// ---------------------------------------------------------------------------
// Scope names
// ---------------------------------------------------------------------------

/** Width of a scope name at font size `size` (CSS: .scope-name, weight 700). */
const nameWidth = (text, size) => text.length * size * 0.62 + size * 0.4;

/**
 * One name per named top scope, largest first, near where most of its
 * memories are (`t.cx`, `t.cy`): the nearest spot (rings of offsets, out to
 * half the scope's reach but at most six name heights) whose box covers no
 * memory, else the one covering fewest (`backed`: the style gives it a faint
 * backing box). Names never overlap each other nor an `avoid` box; one with
 * no spot left at all comes back `placed: false`. Returns the names and
 * their boxes (memory-label obstacles).
 */
function placeScopeNames(tops, n, x, y, r, size, avoid = []) {
  const out = [];
  const boxes = [];
  // Memories by grid cell, for "how many does this box cover".
  const cell = Math.max(S, size * 2);
  const grid = new Map();
  for (let i = 0; i < n; i++) {
    const key = Math.floor(x[i] / cell) + ',' + Math.floor(y[i] / cell);
    let b = grid.get(key);
    if (!b) grid.set(key, (b = []));
    b.push(i);
  }
  const covered = (b, stop) => {
    let c = 0;
    for (let cx = Math.floor((b.x0 - 12) / cell); cx <= Math.floor((b.x1 + 12) / cell); cx++) {
      for (let cy = Math.floor((b.y0 - 12) / cell); cy <= Math.floor((b.y1 + 12) / cell); cy++) {
        for (const i of grid.get(cx + ',' + cy) || []) {
          const rr = r[i] + 3;
          if (x[i] + rr > b.x0 && x[i] - rr < b.x1 && y[i] + rr > b.y0 && y[i] - rr < b.y1 && ++c >= stop) return c;
        }
      }
    }
    return c;
  };
  const hits = (b, o) => b.x0 < o.x1 + 6 && o.x0 - 6 < b.x1 && b.y0 < o.y1 + 4 && o.y0 - 4 < b.y1;
  // Placed names and `avoid` boxes by the grid cells they touch (+ margin).
  const taken = new Map();
  const spanOf = (b, f) => {
    for (let cx = Math.floor((b.x0 - 6) / cell); cx <= Math.floor((b.x1 + 6) / cell); cx++) {
      for (let cy = Math.floor((b.y0 - 4) / cell); cy <= Math.floor((b.y1 + 4) / cell); cy++) f(cx * 65536 + cy);
    }
  };
  const take = (b) =>
    spanOf(b, (key) => {
      let list = taken.get(key);
      if (!list) taken.set(key, (list = []));
      list.push(b);
    });
  for (const b of avoid) take(b);
  const onName = (b) => {
    let hit = false;
    spanOf(b, (key) => {
      if (!hit) for (const o of taken.get(key) || []) if (hits(b, o)) hit = true;
    });
    return hit;
  };
  for (const t of tops) {
    if (!t.key) continue;
    const w = nameWidth(t.key, size);
    const h = size * 1.2;
    // Bounded from the centre: on a dense graph the only free spots are at
    // the rim, and a name there labels nothing; a covered spot near the
    // centre (drawn on a faint backing box) reads better.
    const limit = Math.max(size * 2, Math.min(t.reach * 0.5, size * 6));
    const step = Math.max(size * 0.7, limit / 12);
    const rings = Math.ceil(limit / step);
    let best = null;
    for (let ring = 0; ring <= rings && !(best && best.c === 0); ring++) {
      // Each ring's offsets nearest first, then upward first (a name reads above its group).
      const cand = [];
      if (ring === 0) cand.push([0, 0]);
      else {
        for (let k = -ring; k <= ring; k++) cand.push([k, -ring], [k, ring]);
        for (let k = -ring + 1; k <= ring - 1; k++) cand.push([-ring, k], [ring, k]);
        cand.sort((p, q) => p[0] * p[0] + p[1] * p[1] - (q[0] * q[0] + q[1] * q[1]) || p[1] - q[1] || p[0] - q[0]);
      }
      for (const [ox, oy] of cand) {
        const cx = t.cx + ox * step;
        const cy = t.cy + oy * step * 0.6;
        const b = { x0: cx - w / 2, y0: cy - h / 2, x1: cx + w / 2, y1: cy + h / 2 };
        if (onName(b)) continue;
        const c = covered(b, best ? best.c : Infinity);
        if (!best || c < best.c) best = { c, cx, cy, b };
        if (c === 0) break;
      }
    }
    const cx = best ? best.cx : t.cx;
    const cy = best ? best.cy : t.cy;
    const b = best ? best.b : { x0: cx - w / 2, y0: cy - h / 2, x1: cx + w / 2, y1: cy + h / 2 };
    if (best) {
      boxes.push(b);
      take(b);
    }
    // `y` is the text baseline (dominant-baseline is left alone, for every browser).
    out.push({ key: t.key, slot: t.slot, x: cx, y: cy + size * 0.36, size, w, placed: !!best, backed: !!best && best.c > 0, box: b });
  }
  return { names: out, boxes };
}

/** Whether segments ab and cd cross properly (a shared end does not count). */
function crosses(ax, ay, bx, by, cx, cy, dx, dy) {
  const o = (px, py, qx, qy, rx, ry) => Math.sign((qx - px) * (ry - py) - (qy - py) * (rx - px));
  return o(ax, ay, bx, by, cx, cy) * o(ax, ay, bx, by, dx, dy) < 0 && o(cx, cy, dx, dy, ax, ay) * o(cx, cy, dx, dy, bx, by) < 0;
}

/**
 * The label nudge's test (labels.js `accept` / `moved`). Memory `i` may move
 * to (x, y) only if
 *   - it is not a hub (more than NUDGE_DEGREE relations: its moves are the
 *     costliest and the least needed, a hub's label is placed first);
 *   - it does not end nearer another top scope's centre than its own,
 *     unless it already was and the move takes it no deeper, so a move
 *     never carries it into a neighbour's region;
 *   - its relations then cross no more relations than they do now.
 * Crossings are tested only against relations whose box shares a grid cell
 * with the moved relation's (the grid is kept up to date through `moved`),
 * the current count is worked out once per memory, and every crossing test
 * draws on one budget (NUDGE_WORK): once it is spent, no further move is
 * accepted, so a dense small graph costs bounded time (the declared loss:
 * some labels that a move could have placed stay hover-only).
 */
function nudgeTest(nodes, edges, pos, cell, topOfNode, tops) {
  const n = nodes.length;
  const segs = [];
  const adj = Array.from({ length: n }, () => []);
  for (const e of edges) {
    if (!(e.t >= 0 && e.t < n && e.s >= 0 && e.s < n) || e.t === e.s) continue;
    adj[e.s].push(e.t);
    adj[e.t].push(e.s);
    segs.push([e.s, e.t]);
  }
  const grid = new Map();
  const cellsOf = (ax, ay, bx, by, f) => {
    const cx0 = Math.floor(Math.min(ax, bx) / cell);
    const cx1 = Math.floor(Math.max(ax, bx) / cell);
    const cy0 = Math.floor(Math.min(ay, by) / cell);
    const cy1 = Math.floor(Math.max(ay, by) / cell);
    for (let cx = cx0; cx <= cx1; cx++) for (let cy = cy0; cy <= cy1; cy++) f(cx * 65536 + cy);
  };
  const file = (k) => {
    const [a, b] = segs[k];
    cellsOf(pos[2 * a], pos[2 * a + 1], pos[2 * b], pos[2 * b + 1], (key) => {
      let list = grid.get(key);
      if (!list) grid.set(key, (list = []));
      list.push(k);
    });
  };
  const segsOf = Array.from({ length: n }, () => []);
  segs.forEach(([a, b], k) => {
    segsOf[a].push(k);
    segsOf[b].push(k);
    file(k);
  });
  const stamp = new Int32Array(segs.length);
  let query = 0;
  let work = 0;
  const count = (i, x, y) => {
    let c = 0;
    for (const j of adj[i]) {
      const jx = pos[2 * j];
      const jy = pos[2 * j + 1];
      query++;
      cellsOf(x, y, jx, jy, (key) => {
        for (const k of grid.get(key) || []) {
          if (stamp[k] === query) continue;
          stamp[k] = query;
          const [a, b] = segs[k];
          if (a === i || b === i || a === j || b === j) continue;
          work++;
          if (crosses(x, y, jx, jy, pos[2 * a], pos[2 * a + 1], pos[2 * b], pos[2 * b + 1])) c++;
        }
      });
    }
    return c;
  };
  // How far (x, y) is inside another named scope's territory: the squared
  // distance to its own centre minus that to the nearest other one
  // (> 0: nearer another scope's centre than its own).
  const intrusion = (own, x, y) => {
    let other = Infinity;
    for (let t = 0; t < tops.length; t++) {
      if (t === own || !tops[t].key) continue;
      const dx = x - tops[t].cx;
      const dy = y - tops[t].cy;
      if (dx * dx + dy * dy < other) other = dx * dx + dy * dy;
    }
    const dx = x - tops[own].cx;
    const dy = y - tops[own].cy;
    return dx * dx + dy * dy - other;
  };
  let baseOf = -1;
  let base = 0;
  return {
    accept(i, x, y) {
      if (work > NUDGE_WORK || adj[i].length > NUDGE_DEGREE) return false;
      const own = topOfNode[i];
      if (own >= 0 && tops[own].key) {
        const after = intrusion(own, x, y);
        if (after > 0 && after > intrusion(own, pos[2 * i], pos[2 * i + 1])) return false;
      }
      if (!adj[i].length) return true;
      if (baseOf !== i) {
        baseOf = i;
        base = count(i, pos[2 * i], pos[2 * i + 1]);
      }
      return count(i, x, y) <= base;
    },
    moved(i) {
      // Re-file the moved memory's relations (stale cells only cost a test).
      for (const k of segsOf[i]) file(k);
    },
  };
}

// ---------------------------------------------------------------------------
// The layout
// ---------------------------------------------------------------------------

export function layout(input) {
  const nodes = input.nodes || [];
  const edges = input.edges || [];
  const n = nodes.length;
  const aspect = Math.min(3, Math.max(0.3, input.aspect || 0.62));
  const opt = { ...DEFAULTS, ...(input.options || {}) };
  // Internal index k = rank in id order, so every loop below walks memories in id order.
  const order = [...Array(n).keys()].sort((a, b) => cmp(nodes[a].id, nodes[b].id));
  const rank = new Int32Array(n);
  order.forEach((i, k) => (rank[i] = k));
  const node = (k) => nodes[order[k]];
  const r = new Float64Array(n);
  for (let k = 0; k < n; k++) r[k] = node(k).r || 6;

  // Relations between drawn memories, in endpoint order (stubs and self-loops pull nothing).
  const pairs = [];
  for (const e of edges) {
    if (e.t >= 0 && e.t !== e.s && e.s >= 0 && e.s < n && e.t < n) {
      const a = rank[e.s];
      const b = rank[e.t];
      pairs.push(a < b ? [a, b] : [b, a]);
    }
  }
  pairs.sort((p, q) => p[0] - q[0] || p[1] - q[1]);
  const ea = new Int32Array(pairs.length);
  const eb = new Int32Array(pairs.length);
  const deg = new Float64Array(n);
  pairs.forEach(([a, b], e) => {
    ea[e] = a;
    eb[e] = b;
    deg[a]++;
    deg[b]++;
  });
  const mass = new Float64Array(n);
  for (let k = 0; k < n; k++) mass[k] = 1 + deg[k] * 0.5;

  // ---- scopes: top scopes by size (no scope last), sub-scopes = full scope
  const topIdx = new Map();
  const subIdx = new Map();
  const topOfName = new Array(n);
  for (let k = 0; k < n; k++) {
    const top = node(k).top_scope || '';
    topOfName[k] = top;
    if (!topIdx.has(top)) topIdx.set(top, { key: top, slot: node(k).slot ?? null, members: [] });
    topIdx.get(top).members.push(k);
  }
  const tops = [...topIdx.values()].sort((a, b) => (a.key === '') - (b.key === '') || b.members.length - a.members.length || cmp(a.key, b.key));
  const nTop = tops.length;
  const topOf = new Int32Array(n);
  tops.forEach((t, ti) => {
    t.index = ti;
    for (const k of t.members) topOf[k] = ti;
  });
  // A sub-scope is the full scope when it is deeper than its top scope; the
  // top scope's own memories form the top's "own" group (not a sub-scope).
  const subOf = new Int32Array(n).fill(-1);
  const subs = [];
  for (const t of tops) {
    const groups = new Map();
    for (const k of t.members) {
      const full = node(k).scope || '';
      const key = full && full !== t.key ? full : '';
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(k);
    }
    t.groups = [...groups.entries()]
      .map(([key, members]) => ({ key, members }))
      .sort((a, b) => b.members.length - a.members.length || cmp(a.key, b.key));
    for (const gr of t.groups) {
      if (!gr.key) continue;
      gr.sub = subs.length;
      subs.push(gr);
      for (const k of gr.members) subOf[k] = gr.sub;
    }
  }
  // A scope pulls its memories together as strongly as its relations stay
  // inside it: one whose relations mostly leave would otherwise be held
  // together against them, crossing every one. No scope pulls weaker still.
  const inside = new Float64Array(nTop);
  const ends = new Float64Array(nTop);
  let cross = 0;
  for (let e = 0; e < ea.length; e++) {
    const ta = topOf[ea[e]];
    const tb = topOf[eb[e]];
    ends[ta]++;
    ends[tb]++;
    if (ta === tb) inside[ta] += 2;
    else cross++;
  }
  const topW = new Float64Array(nTop);
  tops.forEach((t, ti) => {
    const kept = ends[ti] ? inside[ti] / ends[ti] : 1;
    topW[ti] = (t.key ? 1 : 0.4) * (0.25 + 0.75 * Math.min(1, kept / 0.6));
  });
  // The more relations cross scopes, the further the start is from the end:
  // more steps, and a hotter start.
  const crossShare = ea.length ? cross / ea.length : 0;

  const x = new Float64Array(n);
  const y = new Float64Array(n);
  if (n) {
    // ---- 1. start: sub-scope discs inside each top scope, then top scopes by affinity
    const groupOf = new Int32Array(n); // index into the top's groups
    for (const t of tops) t.groups.forEach((gr, gi) => gr.members.forEach((k) => (groupOf[k] = gi)));
    const pad = S * 0.6;
    const topAff = new Float64Array(nTop * nTop);
    for (let e = 0; e < ea.length; e++) {
      const ta = topOf[ea[e]];
      const tb = topOf[eb[e]];
      if (ta !== tb) {
        topAff[ta * nTop + tb]++;
        topAff[tb * nTop + ta]++;
      }
    }
    // Per top scope: its groups' affinities, and each group's relations to
    // other tops (group → (other top → relations)). One pass over the relations.
    for (const t of tops) {
      const m = t.groups.length;
      t.aff = m > 1 ? new Float64Array(m * m) : null;
      t.ext = new Map();
    }
    for (let e = 0; e < ea.length; e++) {
      const a = ea[e];
      const b = eb[e];
      const ta = topOf[a];
      const tb = topOf[b];
      if (ta === tb) {
        const t = tops[ta];
        if (t.aff && groupOf[a] !== groupOf[b]) {
          const m = t.groups.length;
          t.aff[groupOf[a] * m + groupOf[b]]++;
          t.aff[groupOf[b] * m + groupOf[a]]++;
        }
      } else {
        const ka = groupOf[a] * nTop + tb;
        tops[ta].ext.set(ka, (tops[ta].ext.get(ka) || 0) + 1);
        const kb = groupOf[b] * nTop + ta;
        tops[tb].ext.set(kb, (tops[tb].ext.get(kb) || 0) + 1);
      }
    }
    for (const t of tops) {
      const m = t.groups.length;
      const rad = new Float64Array(m);
      t.groups.forEach((gr, gi) => (rad[gi] = SEED * Math.sqrt(gr.members.length) + S * 0.3));
      const aff = t.aff;
      t.local = pack(rad, aff, aspect, pad * 0.5);
      // The group's centroid at the top's centre, and its reach (radius).
      let cx = 0;
      let cy = 0;
      let wsum = 0;
      for (let gi = 0; gi < m; gi++) {
        const w = t.groups[gi].members.length;
        cx += t.local[2 * gi] * w;
        cy += t.local[2 * gi + 1] * w;
        wsum += w;
      }
      cx /= wsum;
      cy /= wsum;
      let reach = 0;
      for (let gi = 0; gi < m; gi++) {
        t.local[2 * gi] -= cx;
        t.local[2 * gi + 1] -= cy;
        const lx = t.local[2 * gi];
        const ly = t.local[2 * gi + 1];
        const d = Math.sqrt(lx * lx + ly * ly) + rad[gi];
        if (d > reach) reach = d;
      }
      t.rad = reach;
    }
    const topRad = new Float64Array(nTop);
    tops.forEach((t, ti) => (topRad[ti] = t.rad));
    const centres = pack(topRad, topAff, aspect, pad);
    // Turn each top's sub-scopes to face the tops they relate to (12 turns, 30° apart).
    for (const t of tops) {
      const ti = t.index;
      const tx = centres[2 * ti];
      const ty = centres[2 * ti + 1];
      let bestTurn = 0;
      if (t.groups.length > 1 && t.ext.size) {
        // The heaviest TURN_MAX (group, other top) pairs decide the turn;
        // with many scopes all related the rest change it little and cost
        // 12 distances each.
        let ext = [...t.ext];
        if (ext.length > TURN_MAX) ext = ext.sort((p, q) => q[1] - p[1] || p[0] - q[0]).slice(0, TURN_MAX);
        const ne = ext.length;
        const elx = new Float64Array(ne);
        const ely = new Float64Array(ne);
        const eux = new Float64Array(ne);
        const euy = new Float64Array(ne);
        const ew = new Float64Array(ne);
        let q = 0;
        for (const [key, w] of ext) {
          const gi = Math.floor(key / nTop);
          const u = key - gi * nTop;
          elx[q] = t.local[2 * gi];
          ely[q] = t.local[2 * gi + 1];
          eux[q] = centres[2 * u] - tx;
          euy[q] = centres[2 * u + 1] - ty;
          ew[q++] = w;
        }
        let best = Infinity;
        for (let turn = 0; turn < 12; turn++) {
          const [c, sn] = DIRS[turn];
          let cost = 0;
          for (let e = 0; e < ne; e++) {
            const dx = elx[e] * c - ely[e] * sn - eux[e];
            const dy = elx[e] * sn + ely[e] * c - euy[e];
            cost += ew[e] * Math.sqrt(dx * dx + dy * dy);
          }
          if (cost < best - 1e-9) {
            best = cost;
            bestTurn = turn;
          }
        }
      }
      const [c, sn] = DIRS[bestTurn];
      t.cx = tx;
      t.cy = ty;
      t.reach = t.rad;
      t.groups.forEach((gr, gi) => {
        const lx = t.local[2 * gi];
        const ly = t.local[2 * gi + 1];
        // Most connected in the middle of its disc, then by id.
        const ids = [...gr.members].sort((a, b) => deg[b] - deg[a] || a - b);
        sunflower(ids, x, y, tx + lx * c - ly * sn, ty + lx * sn + ly * c, SEED);
      });
    }
    // ---- 2. simulation
    const iters = Math.round((n <= 60 ? 220 : n <= 300 ? 150 : n <= 1200 ? 110 : 90) * opt.steps * (1 + crossShare));
    if (n > 1) simulate(n, x, y, ea, eb, mass, topOf, nTop, topW, subOf, subs.length, aspect, iters, { ...opt, temp: opt.temp * (1 + 1.2 * crossShare) });
  }

  // ---- 3. spread a small graph for its labels, then remove overlaps. A
  // graph narrower than SPREAD_TO (its width as Fit sees it: the height
  // counts through the box's shape) is scaled up toward it, at most 3×, so
  // Fit draws it near 1:1 on a desktop-sized box (where every label shows)
  // rather than blowing up its memories; a wider one is left alone.
  let spread = 1;
  if (n > 1) {
    let w0 = Infinity;
    let w1 = -Infinity;
    let h0 = Infinity;
    let h1 = -Infinity;
    for (let k = 0; k < n; k++) {
      if (x[k] < w0) w0 = x[k];
      if (x[k] > w1) w1 = x[k];
      if (y[k] < h0) h0 = y[k];
      if (y[k] > h1) h1 = y[k];
    }
    const wide = Math.max(w1 - w0, (h1 - h0) / aspect, S);
    // A short, wide box (aspect under 0.62) gets a wider target: Fit there
    // is bound by the height, and the y-gravity has already made the graph
    // flat, so at SPREAD_TO its labels would crowd.
    const target = SPREAD_TO * Math.max(1, Math.sqrt(0.62 / aspect));
    spread = Math.max(1, Math.min(3, target / wide));
    if (spread > 1) {
      // Fill the box's shape: labels are wide, so a graph that fills the
      // width as well as the height keeps more of them apart. A stretch
      // is affine, so it adds no crossing; bounded so shapes stay true.
      const sx = Math.min(spread * 1.35, Math.max(spread, target / Math.max(S, w1 - w0)));
      const sy = Math.min(spread, Math.max(spread / 1.35, (target * aspect) / Math.max(S, h1 - h0)));
      for (let k = 0; k < n; k++) {
        x[k] *= sx;
        y[k] *= sy;
      }
    }
  }
  separate(n, x, y, r, 6);

  // ---- 4. scope names
  let ex0 = Infinity;
  let ex1 = -Infinity;
  let ey0 = Infinity;
  let ey1 = -Infinity;
  for (let k = 0; k < n; k++) {
    ex0 = Math.min(ex0, x[k]);
    ex1 = Math.max(ex1, x[k]);
    ey0 = Math.min(ey0, y[k]);
    ey1 = Math.max(ey1, y[k]);
  }
  // Sized for the whole graph: about 1/70 of the width Fit maps to the box.
  // This size, which does not depend on the box's width, is the one the
  // memory labels (and their nudges) avoid, so positions never depend on
  // the width; the drawn size (step 5b) may be larger.
  const extent = n ? Math.max(ex1 - ex0, (ey1 - ey0) / aspect) : 0;
  const nameSize = Math.min(120, Math.max(15, extent / 70));
  // Each scope's name goes where most of its memories are: the centre of
  // its members in the densest 3 × 3 block of BLOCK-sized cells (a scope
  // the relations pulled apart has no useful mean), and its reach is how far
  // its members spread from there.
  const block = S * 3 * spread;
  for (const t of tops) {
    const cells = new Map();
    for (const k of t.members) {
      const key = Math.floor(x[k] / block) * 65536 + Math.floor(y[k] / block);
      cells.set(key, (cells.get(key) || 0) + 1);
    }
    let bestKey = 0;
    let bestCount = -1;
    for (const k of t.members) {
      const bx = Math.floor(x[k] / block);
      const by = Math.floor(y[k] / block);
      let c = 0;
      for (let ox = -1; ox <= 1; ox++) for (let oy = -1; oy <= 1; oy++) c += cells.get((bx + ox) * 65536 + by + oy) || 0;
      if (c > bestCount) {
        bestCount = c;
        bestKey = k;
      }
    }
    const bx = Math.floor(x[bestKey] / block);
    const by = Math.floor(y[bestKey] / block);
    let cx = 0;
    let cy = 0;
    let m = 0;
    for (const k of t.members) {
      if (Math.abs(Math.floor(x[k] / block) - bx) <= 1 && Math.abs(Math.floor(y[k] / block) - by) <= 1) {
        cx += x[k];
        cy += y[k];
        m++;
      }
    }
    t.cx = cx / m;
    t.cy = cy / m;
    let reach = 0;
    for (const k of t.members) {
      const dx = x[k] - t.cx;
      const dy = y[k] - t.cy;
      if (dx * dx + dy * dy > reach) reach = dx * dx + dy * dy;
    }
    t.reach = Math.sqrt(reach) + S;
  }
  const { names, boxes } = placeScopeNames(tops, n, x, y, r, nameSize);

  // ---- 5. memory labels, in the caller's node order
  const pos = new Array(n * 2);
  for (let i = 0; i < n; i++) {
    pos[2 * i] = x[rank[i]];
    pos[2 * i + 1] = y[rank[i]];
  }
  // A label with no free spot may move its memory up to about a spacing
  // (labels.js `nudge`, bounded by `nudgeTest`); read the moved positions back.
  const nudge = n <= NUDGE_MAX ? S * Math.min(spread, 1.5) : 0;
  let test = null;
  if (nudge) {
    const topOfNode = new Int32Array(n);
    for (let i = 0; i < n; i++) topOfNode[i] = topOf[rank[i]];
    test = nudgeTest(nodes, edges, pos, S * 2 * spread, topOfNode, tops);
  }
  const labels = placeLabels(nodes, pos, boxes, { corners: true, nudge, accept: test && test.accept, moved: test && test.moved });
  for (let i = 0; i < n; i++) {
    x[rank[i]] = pos[2 * i];
    y[rank[i]] = pos[2 * i + 1];
  }

  // ---- 5b. the drawn scope names: at least 12 px on screen at Fit in a box
  // `input.width` wide (a phone), so a name stays readable at Fit whatever
  // the store's size. Only the names move with the width: placed again at
  // that size clear of the placed labels, each keeping its step-4 spot
  // when no new one is free.
  const boxW = Math.max(200, input.width || 900);
  const drawSize = Math.min(120, Math.max(nameSize, (12 * (extent + 200)) / boxW));
  let shown = names;
  if (n && drawSize > nameSize * 1.01) {
    const avoid = [];
    for (const l of labels) if (l.placed) avoid.push(l.box);
    const big = placeScopeNames(tops, n, x, y, r, drawSize, avoid).names;
    shown = big.map((nm, k) => (nm.placed ? nm : names[k]));
  }

  // ---- 6. routes: straight; stubs toward their far scope, else away from neighbours
  const centreOf = new Map(tops.filter((t) => t.key).map((t) => [t.key, t]));
  const stubCount = new Int32Array(n);
  const stubLen = S * 1.5 * spread;
  // Nearby memories, for "away from the neighbours".
  const near = new Map();
  const ncell = S * 2 * spread;
  for (let k = 0; k < n; k++) {
    const key = Math.floor(x[k] / ncell) + ',' + Math.floor(y[k] / ncell);
    if (!near.has(key)) near.set(key, []);
    near.get(key).push(k);
  }
  const awayFrom = (k) => {
    let vx = 0;
    let vy = 0;
    const cx = Math.floor(x[k] / ncell);
    const cy = Math.floor(y[k] / ncell);
    for (let ox = -1; ox <= 1; ox++) {
      for (let oy = -1; oy <= 1; oy++) {
        for (const j of near.get(cx + ox + ',' + (cy + oy)) || []) {
          if (j === k) continue;
          const dx = x[k] - x[j];
          const dy = y[k] - y[j];
          const d2 = dx * dx + dy * dy;
          if (d2 > ncell * ncell || d2 < 1e-9) continue;
          vx += dx / d2;
          vy += dy / d2;
        }
      }
    }
    return [vx, vy];
  };
  const routes = edges.map((e) => {
    const s = e.s;
    if (!(s >= 0 && s < n)) return [[0, 0], [0, 0]];
    const k = rank[s];
    const a = [x[k], y[k]];
    if (e.t >= 0 && e.t < n) return [a, [x[rank[e.t]], y[rank[e.t]]]];
    let vx = 0;
    let vy = 0;
    const far = e.far_scope ? centreOf.get(e.far_scope) : null;
    if (far && far.key !== topOfName[k]) {
      vx = far.cx - x[k];
      vy = far.cy - y[k];
    } else {
      [vx, vy] = awayFrom(k);
    }
    let len = Math.sqrt(vx * vx + vy * vy);
    if (len < 1e-9) {
      [vx, vy] = DIRS[10]; // up and to the right
      len = 1;
    }
    vx /= len;
    vy /= len;
    // The j-th stub on a memory turns 30° × ⌈j/2⌉, alternately each way.
    const j = stubCount[k]++;
    const turn = ((j + 1) >> 1) * (j % 2 ? 1 : -1);
    const [c, sn] = DIRS[((turn % 12) + 12) % 12];
    const ux = vx * c - vy * sn;
    const uy = vx * sn + vy * c;
    return [a, [x[k] + ux * stubLen, y[k] + uy * stubLen]];
  });

  // ---- soft regions: about half the typical gap between neighbours
  let regionR = S * 0.9 * spread;
  if (n > 2) {
    const nn = [];
    for (let k = 0; k < n; k++) {
      let best = Infinity;
      const cx = Math.floor(x[k] / ncell);
      const cy = Math.floor(y[k] / ncell);
      for (let ox = -1; ox <= 1; ox++) {
        for (let oy = -1; oy <= 1; oy++) {
          for (const j of near.get(cx + ox + ',' + (cy + oy)) || []) {
            if (j === k) continue;
            const dx = x[k] - x[j];
            const dy = y[k] - y[j];
            const d2 = dx * dx + dy * dy;
            if (d2 < best) best = d2;
          }
        }
      }
      if (best < Infinity) nn.push(Math.sqrt(best));
    }
    if (nn.length) {
      nn.sort((a, b) => a - b);
      // Wider on a dense graph, so its regions merge instead of ringing each memory.
      const f = 0.5 + 0.5 * Math.min(1, n / 600);
      regionR = Math.min(S * 1.2 * spread, Math.max(S * 0.5, nn[nn.length >> 1] * f));
    }
  }

  // ---- bounds: memories, routes, placed labels and names, regions
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  const grow = (gx, gy) => {
    if (gx < minX) minX = gx;
    if (gx > maxX) maxX = gx;
    if (gy < minY) minY = gy;
    if (gy > maxY) maxY = gy;
  };
  for (let k = 0; k < n; k++) {
    const rr = Math.max(r[k] + 6, regionR);
    grow(x[k] - rr, y[k] - rr);
    grow(x[k] + rr, y[k] + rr);
  }
  for (const rt of routes) for (const p of rt) grow(p[0], p[1]);
  for (const l of labels) {
    if (!l.placed) continue;
    grow(l.box.x0, l.box.y0);
    grow(l.box.x1, l.box.y1);
  }
  for (const nm of shown) {
    if (!nm.placed) continue;
    grow(nm.box.x0, nm.box.y0);
    grow(nm.box.x1, nm.box.y1);
  }
  if (!n) {
    minX = minY = 0;
    maxX = maxY = S * 2;
  }
  const padB = 16;
  return {
    pos,
    routes,
    labels: labels.map((l) => ({ x: l.x, y: l.y, w: l.w, anchor: l.anchor, placed: l.placed })),
    bounds: { x: minX - padB, y: minY - padB, w: maxX - minX + padB * 2, h: maxY - minY + padB * 2 },
    scopes: shown.map((nm) => ({ key: nm.key, slot: nm.slot, x: nm.x, y: nm.y, w: nm.w, size: nm.size, placed: nm.placed, backed: nm.backed })),
    region_r: regionR,
  };
}
