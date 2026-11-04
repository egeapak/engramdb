// Collision-free memory labels, for any layout (core, so every style's
// layout places labels the same way and none has to borrow another's code).
//
// Most important memory first (degree, then reads, then id), each label
// takes the first free spot of below / above / right / left (then, when a
// layout asks, the four corners, and a small move of the memory) that hits
// no memory, no obstacle and no label already placed. A label with no free
// spot is still returned, at its first spot with `placed: false`: the view draws
// it hidden and shows it only while its memory is highlighted, so hovering a
// memory (or a neighbour of a pinned one) always names it.
//
// Deterministic and pure: plain data in, plain data out (it runs in the
// layout worker).

/** Label metrics in layout units (CSS: .graph .lbl, 11.5px, weight 600). */
export const LABEL_H = 14;
const CHAR_W = 6.6;
export const labelWidth = (text) => Math.ceil(String(text).length * CHAR_W) + 4;

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/**
 * `nodes`: [{ id, r, label, degree, weight }]; `pos`: [x0, y0, x1, y1, …];
 * `obstacles`: [{ x0, y0, x1, y1 }] a label must avoid besides the memories
 * (Grid: its scope badges; the scope-force layout: its scope names).
 * `opts.corners`: also try the four corners (below / above, to the right /
 * left) after the four sides, for a layout whose memories are not on a
 * grid and so often leave a corner free when every side is taken. Off by
 * default, so a layout that does not ask places exactly as before.
 * `opts.nudge`: when no spot is free, move the memory up to this far
 * (layout units, 16 tries) to one where a spot is, writing its new position
 * into `pos` — for a layout that has no grid to keep. Off by default.
 * `opts.accept(i, x, y)`: whether memory `i` may move there (the layout's
 * own test, e.g. that it crosses no more relations than before), asked only
 * for a move that would place the label; `opts.moved(i, x, y)` is told of
 * each move made, after `pos` is updated.
 * Returns one `{ x, y, w, anchor, placed, box }` per node.
 */
export function placeLabels(nodes, pos, obstacles = [], opts = {}) {
  const n = nodes.length;
  const out = new Array(n).fill(null);
  const CELL = 120;
  const grid = new Map();
  const keyOf = (cx, cy) => cx + ',' + cy;
  const add = (b) => {
    for (let cx = Math.floor(b.x0 / CELL); cx <= Math.floor(b.x1 / CELL); cx++) {
      for (let cy = Math.floor(b.y0 / CELL); cy <= Math.floor(b.y1 / CELL); cy++) {
        const k = keyOf(cx, cy);
        if (!grid.has(k)) grid.set(k, []);
        grid.get(k).push(b);
      }
    }
  };
  const hits = (b) => {
    for (let cx = Math.floor(b.x0 / CELL); cx <= Math.floor(b.x1 / CELL); cx++) {
      for (let cy = Math.floor(b.y0 / CELL); cy <= Math.floor(b.y1 / CELL); cy++) {
        for (const o of grid.get(keyOf(cx, cy)) || []) {
          if (!o.dead && b.x0 < o.x1 && o.x0 < b.x1 && b.y0 < o.y1 && o.y0 < b.y1) return true;
        }
      }
    }
    return false;
  };
  // Every memory is an obstacle first, then the caller's.
  const discs = new Array(n);
  for (let i = 0; i < n; i++) {
    const r = (nodes[i].r || 6) + 3;
    const x = pos[2 * i];
    const y = pos[2 * i + 1];
    discs[i] = { x0: x - r, y0: y - r, x1: x + r, y1: y + r };
    add(discs[i]);
  }
  for (const b of obstacles) add(b);
  const order = [...Array(n).keys()].sort(
    (a, b) => (nodes[b].degree || 0) - (nodes[a].degree || 0) || (nodes[b].weight || 0) - (nodes[a].weight || 0) || cmp(nodes[a].id, nodes[b].id),
  );
  const spotsAt = (x, y, r, w) => {
    const spots = [
      { x, y: y + r + LABEL_H - 2, anchor: 'middle', b: { x0: x - w / 2, y0: y + r, x1: x + w / 2, y1: y + r + LABEL_H } },
      { x, y: y - r - 4, anchor: 'middle', b: { x0: x - w / 2, y0: y - r - LABEL_H, x1: x + w / 2, y1: y - r } },
      { x: x + r + 2, y: y + 4, anchor: 'start', b: { x0: x + r, y0: y - LABEL_H / 2, x1: x + r + 2 + w, y1: y + LABEL_H / 2 } },
      { x: x - r - 2, y: y + 4, anchor: 'end', b: { x0: x - r - 2 - w, y0: y - LABEL_H / 2, x1: x - r, y1: y + LABEL_H / 2 } },
    ];
    if (opts.corners) {
      const c = r * 0.5;
      spots.push(
        { x: x + c, y: y + r + LABEL_H - 2, anchor: 'start', b: { x0: x + c, y0: y + r, x1: x + c + w, y1: y + r + LABEL_H } },
        { x: x - c, y: y + r + LABEL_H - 2, anchor: 'end', b: { x0: x - c - w, y0: y + r, x1: x - c, y1: y + r + LABEL_H } },
        { x: x + c, y: y - r - 4, anchor: 'start', b: { x0: x + c, y0: y - r - LABEL_H, x1: x + c + w, y1: y - r } },
        { x: x - c, y: y - r - 4, anchor: 'end', b: { x0: x - c - w, y0: y - r - LABEL_H, x1: x - c, y1: y - r } },
      );
    }
    return spots;
  };
  for (const i of order) {
    const text = nodes[i].label || '';
    const w = labelWidth(text);
    const r = (nodes[i].r || 6) + 4;
    const x = pos[2 * i];
    const y = pos[2 * i + 1];
    const spots = spotsAt(x, y, r, w);
    let free = text ? spots.find((sp) => !hits(sp.b)) : null;
    if (!free && text && opts.nudge) {
      // Move the memory a little to where one of its spots is free: its own
      // disc stops counting, the moved disc must hit nothing either.
      discs[i].dead = true;
      const rd = (nodes[i].r || 6) + 3;
      for (const [ux, uy] of NUDGES) {
        const nx = x + ux * opts.nudge;
        const ny = y + uy * opts.nudge;
        const disc = { x0: nx - rd, y0: ny - rd, x1: nx + rd, y1: ny + rd };
        if (hits(disc)) continue;
        // The caller's test runs last (it may be the costly one), and only
        // for a move that would place the label.
        free = spotsAt(nx, ny, r, w).find((sp) => !hits(sp.b)) || null;
        if (free && opts.accept && !opts.accept(i, nx, ny)) free = null;
        if (free) {
          pos[2 * i] = nx;
          pos[2 * i + 1] = ny;
          discs[i] = disc;
          add(disc);
          if (opts.moved) opts.moved(i, nx, ny);
          break;
        }
      }
      if (!free) discs[i].dead = false;
    }
    const sp = free || spots[0];
    if (free) add(sp.b);
    out[i] = { x: sp.x, y: sp.y, w, anchor: sp.anchor, placed: !!free, box: sp.b };
  }
  return out;
}

/** Nudge directions and lengths (a fraction of `opts.nudge`), nearest first; literals, so every engine agrees. */
const NUDGES = (() => {
  const d = 0.7071067811865476;
  const unit = [[0, 1], [0, -1], [1, 0], [-1, 0], [d, d], [-d, d], [d, -d], [-d, -d]];
  const out = [];
  for (const f of [0.5, 1]) for (const [ux, uy] of unit) out.push([ux * f, uy * f]);
  return out;
})();
