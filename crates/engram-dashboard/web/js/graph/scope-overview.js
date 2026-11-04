// The scope overview's layout: one disc per top scope, area by its
// memories, placed by scope affinity, in screen pixels. A core layout (no
// style draws the overview), run by views/graph-overview.js through the
// LAYOUTS table like any other, so it is cached and checked the same way.
//
// Input (graph/layouts.js's shape, with one node per scope):
//   { layout: 'scope-overview',
//     nodes: [{ id: scope name ('' for none), count, label }],
//     edges: [{ s, t, n }]  one per related scope pair (s ≠ t), `n` its
//                           relations under no display filter, so toggling
//                           a kind never moves a disc,
//     aspect: box height / width, width: the box's width, rounded to 40 }
//
// Output: the contract in graph/layouts.js, in pixels of a `width` ×
// `width · aspect` box:
//   pos:    disc centres; r: disc radii (area ∝ memories, at least MIN_R).
//   routes: per pair, [start, control, end]: from one disc's rim to the
//           other's, bowed a little to one side so two bands leaving a disc
//           in about the same direction part (draw it as a quadratic curve).
//   labels: "name · count" above the disc, else below, right or left; one
//           with no free spot is `placed: false` (shown while focused).
//   bounds: the whole box.
//
// Packing reuses scope-force.js's `pack` (affinity first, compact for the
// box shape). Deterministic, with scope-force's arithmetic rules: no random
// numbers, discs in (count, name) order, and only + − × ÷ and Math.sqrt (a
// Rust test checks the file).

import { pack } from './scope-force.js';

/** Pixel padding around the packed discs, and the label line's height. */
const MARGIN = 14;
const LABEL_H = 18;
/** Label text metrics (CSS: .so-name, 12.5px; the count is lighter but as wide). */
const CHAR_W = 7.6;
/** The smallest disc drawn (a one-memory scope beside a 3 000-memory one). */
const MIN_R = 6;
/** The largest disc's radius, as a share of the box's shorter side, so three scopes do not fill the box. */
const MAX_R = 0.3;

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

export function layout(input) {
  const nodes = input.nodes || [];
  const m = nodes.length;
  const aspect = Math.min(3, Math.max(0.3, input.aspect || 0.62));
  const W = input.width || 800;
  const H = Math.round(W * aspect);
  const order = [...Array(m).keys()].sort((a, b) => (nodes[b].count || 0) - (nodes[a].count || 0) || cmp(nodes[a].id, nodes[b].id));
  const rank = new Int32Array(m);
  order.forEach((i, k) => (rank[i] = k));
  // Unit radii: √count, so area follows memories; the padding leaves room
  // for a band between two discs and a label line above each.
  const rad = new Float64Array(m);
  let mean = 0;
  for (let k = 0; k < m; k++) {
    rad[k] = Math.sqrt(Math.max(1, nodes[order[k]].count || 0));
    mean += rad[k];
  }
  mean = m ? mean / m : 1;
  const aff = new Float64Array(m * m);
  for (const e of input.edges || []) {
    if (e.s === e.t || e.s < 0 || e.t < 0 || e.s >= m || e.t >= m) continue;
    const a = rank[e.s];
    const b = rank[e.t];
    aff[a * m + b] += e.n || 1;
    aff[b * m + a] += e.n || 1;
  }
  // A gap of about two mean radii between touching discs: room for a band
  // to read as a band (and for its ⚠) rather than a seam.
  const c = pack(rad, aff, aspect, Math.max(2, mean * 2.2));
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (let k = 0; k < m; k++) {
    x0 = Math.min(x0, c[2 * k] - rad[k]);
    x1 = Math.max(x1, c[2 * k] + rad[k]);
    y0 = Math.min(y0, c[2 * k + 1] - rad[k]);
    y1 = Math.max(y1, c[2 * k + 1] + rad[k]);
  }
  if (!m) x0 = y0 = x1 = y1 = 0;
  const bw = Math.max(1e-6, x1 - x0);
  const bh = Math.max(1e-6, y1 - y0);
  // Scale the pack into the box, a label line clear at the top and bottom.
  let big = 1;
  for (let k = 0; k < m; k++) big = Math.max(big, rad[k]);
  const sc = Math.max(1e-6, Math.min((MAX_R * Math.min(W, H)) / big, (W - 2 * MARGIN) / bw, (H - 2 * MARGIN - 2 * LABEL_H) / bh));
  const ox = (W - bw * sc) / 2 - x0 * sc;
  const oy = (H - bh * sc) / 2 - y0 * sc;
  const pos = new Array(2 * m);
  const r = new Array(m);
  for (let i = 0; i < m; i++) {
    const k = rank[i];
    pos[2 * i] = ox + c[2 * k] * sc;
    pos[2 * i + 1] = oy + c[2 * k + 1] * sc;
    r[i] = Math.max(MIN_R, rad[k] * sc);
  }

  // Bands: rim to rim, the control point pushed off the chord by up to 16px.
  const routes = (input.edges || []).map((e) => {
    const ax = pos[2 * e.s];
    const ay = pos[2 * e.s + 1];
    const tx = pos[2 * e.t];
    const ty = pos[2 * e.t + 1];
    const dx = tx - ax;
    const dy = ty - ay;
    const len = Math.sqrt(dx * dx + dy * dy) || 1;
    const ux = dx / len;
    const uy = dy / len;
    const p0 = [ax + ux * (r[e.s] + 2), ay + uy * (r[e.s] + 2)];
    const p2 = [tx - ux * (r[e.t] + 2), ty - uy * (r[e.t] + 2)];
    const bow = Math.min(16, len * 0.06);
    return [p0, [(p0[0] + p2[0]) / 2 - uy * bow, (p0[1] + p2[1]) / 2 + ux * bow], p2];
  });

  // Labels, biggest scope first: the first free side of above, below,
  // right, left that overlaps no disc (bar its own) and no placed label.
  const boxes = [];
  const labels = new Array(m);
  const free = (bx, by, w, own) => {
    if (bx < 2 || by < 2 || bx + w > W - 2 || by + LABEL_H > H - 2) return false;
    for (const q of boxes) if (bx < q[0] + q[2] && bx + w > q[0] && by < q[1] + LABEL_H && by + LABEL_H > q[1]) return false;
    for (let j = 0; j < m; j++) {
      if (j === own) continue;
      const nx = Math.max(bx, Math.min(pos[2 * j], bx + w));
      const ny = Math.max(by, Math.min(pos[2 * j + 1], by + LABEL_H));
      const ddx = nx - pos[2 * j];
      const ddy = ny - pos[2 * j + 1];
      if (ddx * ddx + ddy * ddy < r[j] * r[j]) return false;
    }
    return true;
  };
  for (const i of order) {
    const nd = nodes[i];
    const text = nd.label || `${nd.id} · ${nd.count}`;
    const w = Math.ceil(text.length * CHAR_W) + 6;
    const x = pos[2 * i];
    const y = pos[2 * i + 1];
    const rr = r[i];
    // [box left, box top, text x, text baseline y, anchor]
    const spots = [
      [x - w / 2, y - rr - 3 - LABEL_H, x, y - rr - 7, 'middle'],
      [x - w / 2, y + rr + 3, x, y + rr + 3 + LABEL_H - 5, 'middle'],
      [x + rr + 5, y - LABEL_H / 2, x + rr + 8, y + 4.5, 'start'],
      [x - rr - 5 - w, y - LABEL_H / 2, x - rr - 8, y + 4.5, 'end'],
    ];
    let pick = null;
    for (const sp of spots) {
      if (free(sp[0], sp[1], w, i)) {
        pick = sp;
        break;
      }
    }
    const sp = pick || spots[0];
    if (pick) boxes.push([sp[0], sp[1], w]);
    labels[i] = { x: sp[2], y: sp[3], w, anchor: sp[4], placed: !!pick };
  }
  return { pos, r, routes, labels, bounds: { x: 0, y: 0, w: W, h: H } };
}
