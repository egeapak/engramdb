// Hit testing for the canvas surface (./host.js), on the CPU: uniform grids
// in layout units, so a pointer move looks at a few cells, never at every
// memory or relation. The SVG surface gets the same answers from the
// browser's own hit testing; these grids reproduce its order (a memory over
// any relation, the later relation over the earlier, as the SVG stacks them
// in index order).

const NODE_CELL = 32;
const EDGE_CELL = 40;

/** A cell's key: two signed cell indices in one number. */
const key = (ix, iy) => (ix + 0x8000) * 0x10000 + (iy + 0x8000);

/**
 * The memories at (x, y): `pos` the layout's flat positions, `reach[i]` how
 * far memory i catches the pointer (its radius plus its ring). Returns the
 * topmost (highest index) memory within reach, or -1.
 */
export function nodeGrid(pos, reach) {
  const cells = new Map();
  let most = 0;
  for (let i = 0; i < reach.length; i++) {
    most = Math.max(most, reach[i]);
    const k = key(Math.floor(pos[2 * i] / NODE_CELL), Math.floor(pos[2 * i + 1] / NODE_CELL));
    let c = cells.get(k);
    if (!c) cells.set(k, (c = []));
    c.push(i);
  }
  const span = Math.ceil(most / NODE_CELL);
  return (x, y) => {
    const cx = Math.floor(x / NODE_CELL);
    const cy = Math.floor(y / NODE_CELL);
    let best = -1;
    for (let dx = -span; dx <= span; dx++)
      for (let dy = -span; dy <= span; dy++) {
        const c = cells.get(key(cx + dx, cy + dy));
        if (!c) continue;
        for (const i of c) {
          if (i <= best) continue;
          const ex = pos[2 * i] - x;
          const ey = pos[2 * i + 1] - y;
          if (ex * ex + ey * ey <= reach[i] * reach[i]) best = i;
        }
      }
    return best;
  };
}

/** Distance squared from (x, y) to the segment a–b. */
function segDist2(x, y, ax, ay, bx, by) {
  const dx = bx - ax;
  const dy = by - ay;
  const l2 = dx * dx + dy * dy;
  let t = l2 ? ((x - ax) * dx + (y - ay) * dy) / l2 : 0;
  t = Math.max(0, Math.min(1, t));
  const ex = ax + t * dx - x;
  const ey = ay + t * dy - y;
  return ex * ex + ey * ey;
}

/**
 * The relations at (x, y): `paths[k]` relation k's polyline ([[x, y], …])
 * or null when it is not drawn. Each segment is sampled every half cell
 * into the cell of each sample, so a point within `tol` of a segment is
 * within `tol` + a quarter cell of some sample: the query looks that far.
 * Returns a function (x, y, tol) → the topmost (highest index) relation
 * whose polyline passes within `tol`, or -1.
 */
export function edgeGrid(paths) {
  const cells = new Map();
  const step = EDGE_CELL / 2;
  paths.forEach((pts, k) => {
    if (!pts) return;
    let last = -1;
    const add = (px, py) => {
      const c = key(Math.floor(px / EDGE_CELL), Math.floor(py / EDGE_CELL));
      if (c === last) return;
      last = c;
      let list = cells.get(c);
      if (!list) cells.set(c, (list = []));
      if (list[list.length - 1] !== k) list.push(k);
    };
    for (let j = 1; j < pts.length; j++) {
      const [ax, ay] = pts[j - 1];
      const [bx, by] = pts[j];
      const len = Math.sqrt((bx - ax) * (bx - ax) + (by - ay) * (by - ay));
      const n = Math.max(1, Math.ceil(len / step));
      for (let s = 0; s <= n; s++) add(ax + ((bx - ax) * s) / n, ay + ((by - ay) * s) / n);
    }
  });
  return (x, y, tol) => {
    const reach = tol + EDGE_CELL / 4;
    const x0 = Math.floor((x - reach) / EDGE_CELL);
    const x1 = Math.floor((x + reach) / EDGE_CELL);
    const y0 = Math.floor((y - reach) / EDGE_CELL);
    const y1 = Math.floor((y + reach) / EDGE_CELL);
    const t2 = tol * tol;
    let best = -1;
    for (let cx = x0; cx <= x1; cx++)
      for (let cy = y0; cy <= y1; cy++) {
        const list = cells.get(key(cx, cy));
        if (!list) continue;
        for (const k of list) {
          if (k <= best) continue;
          const pts = paths[k];
          for (let j = 1; j < pts.length; j++) {
            if (segDist2(x, y, pts[j - 1][0], pts[j - 1][1], pts[j][0], pts[j][1]) <= t2) {
              best = k;
              break;
            }
          }
        }
      }
    return best;
  };
}
