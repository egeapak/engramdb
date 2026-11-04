// The Plain style's relation drawing, without arrowheads. One function,
// `relationPath`, builds every relation's geometry: the graph's relations
// (`drawRelation`), the legend's and every other page's samples
// (`relationSample`, `stubSwatch`) all call it, so a legend can never drift
// from the graph.
//
// Visual language (colours are tokens in css/themes/plain.css):
//   direction     a tapered wedge, wide at the source (3× the target width),
//                 narrowing to the target; symmetric kinds are untapered
//   supports      green solid wedge
//   derived_from  blue ribbed wedge (a spine with cross ribs), so it never
//                 relies on telling blue from green or violet
//   supersedes    dark heavy wedge (the ink colour)
//   contradicts   red zigzag
//   duplicates    violet double stroke
//   related_to    thin grey stroke
//   proposed      dashed, with one mid-relation chevron pointing source →
//                 target (directed kinds only; symmetric ones are dashed only)
//   rejected      faint and dotted (drawn only with the Rejected filter on)
//   stub          the same drawing fading toward the far end; a relation to
//                 a deleted memory ends in a short bar, one into a store not
//                 checked here simply fades out
// Every kind differs by shape as well as colour (wedge / ribbed wedge /
// heavy wedge / zigzag / double / thin), so colour is never the only cue.
//
// **Geometry.** Everything is filled bands in a local frame: x runs along
// the relation from the source's edge (0) to the target's edge (L), y is
// across it, in screen pixels at scale 1. A relation is ONE `<path>`: its
// dashes, chevron and end bar are subpaths of the same path data. On the
// graph the path is placed by a CSS transform, `translate rotate
// scale(1, var(--zw) × var(--zk))`, where `--zw` is set once per zoom on the
// `<svg>` (see `zoomFactor`), so a zoom rescales every relation's width with
// no per-relation work in JS, and `--zk` thickens a highlighted one. Lengths
// along the relation (dash, zigzag period, chevron depth) are sized for the
// zoom they were drawn at and follow the map from there; they are redrawn
// with the relations (filter toggles, a new layout).

import { s } from '../../../dom.js';
import { DIRECTED, edgeClass } from '../../../relations.js';

/** Widths across a relation, in screen px at scale 1 (before `--zw`). */
const W = {
  target: 1.5, // a wedge's narrow end
  source: 4.5, // its wide end: 3× the target
  heavy: 1.45, // supersedes: every wedge width × this
  spine: 1.1, // derived_from's spine
  thin: 1.25, // related_to
  double: 1.0, // each of duplicates' two strokes
  doubleGap: 1.7, // their centres sit at ± this
  zig: 2.6, // contradicts' amplitude
  zigBand: 2.1, // its vertical thickness
  chev: 4.2, // the chevron's half-height
  bar: 4.5, // a dangling stub's end bar, half-height
};

/** Lengths along a relation, in screen px at the zoom it is drawn at. */
const A = { dash: 6, gap: 4, dot: 1.6, dotGap: 3.4, zigHalf: 3.4, chevDepth: 4.2, chevThick: 1.8, chevClear: 3, rib: 3.6, ribW: 1.5, bar: 1.6 };

/**
 * `--zw` for a zoom `scale` (screen px per layout unit): widths grow as
 * scale^0.4 on screen, so a relation stays legible zoomed out and thickens a
 * little zoomed in, instead of following the map 1:1.
 */
export function zoomFactor(scale) {
  const z = Math.pow(Math.max(scale, 1e-3), -0.6);
  return Math.min(8, Math.max(0.25, z));
}

const f1 = (v) => (Math.round(v * 10) / 10).toString();
const f2 = (v) => (Math.round(v * 100) / 100).toString();

/** The dash intervals [[a, b], …] along [0, L] for a status. */
function intervals(status, L, k, hole, teeth) {
  let on;
  let off;
  // A dashed zigzag breaks between whole teeth (`teeth`: its knot spacing),
  // so a dash never ends mid-tooth and the zigzag still reads as one.
  if (status === 'proposed' && teeth) [on, off] = [3 * teeth, teeth];
  else if (status === 'proposed') [on, off] = [A.dash * k, A.gap * k];
  else if (status === 'rejected') [on, off] = [A.dot * k, A.dotGap * k];
  else return hole ? [[0, hole[0]], [hole[1], L]] : [[0, L]];
  // Centre the pattern so both ends get the same partial dash.
  const period = on + off;
  const n = Math.max(1, Math.round((L + off) / period));
  const scaleTo = teeth && status === 'proposed' ? 1 : (L + off) / (n * period);
  const out = [];
  for (let i = 0; i < n; i++) {
    const a = i * period * scaleTo;
    const b = Math.min(L, a + on * scaleTo);
    if (!hole) out.push([a, b]);
    else {
      // Leave the chevron's stretch clear.
      if (b <= hole[0] || a >= hole[1]) out.push([a, b]);
      else {
        if (a < hole[0]) out.push([a, hole[0]]);
        if (b > hole[1]) out.push([hole[1], b]);
      }
    }
  }
  return out;
}

/** How many half-periods a zigzag of length L has. */
const zigCount = (L, k) => Math.max(2, Math.round(L / (A.zigHalf * k)));

/**
 * A band's polygon over [a, b]: `c(x)` its centre, `hw(x)` its half-width,
 * `knots` the x of every bend of the centre (the zigzag's), all in the local
 * frame. Linear between knots, so the polygon is exact.
 */
function band(a, b, c, hw, knots) {
  const xs = [a, ...knots.filter((x) => x > a && x < b), b];
  const top = xs.map((x) => `${f1(x)},${f2(c(x) - hw(x))}`);
  const bot = xs.reverse().map((x) => `${f1(x)},${f2(c(x) + hw(x))}`);
  return 'M' + top.join('L') + 'L' + bot.join('L') + 'Z';
}

/**
 * The bands that draw `kind` over length L: [{ c, hw, knots, from, to }].
 * `k` scales lengths along the relation (zigzag period, rib spacing).
 */
function bandsOf(kind, L, k) {
  const zero = () => 0;
  const lin = (h0, h1) => (x) => h0 + ((h1 - h0) * x) / (L || 1);
  if (kind === 'contradicts') {
    const n = zigCount(L, k);
    const knots = [];
    for (let i = 1; i < n; i++) knots.push((L * i) / n);
    const c = (x) => {
      // Triangle wave through 0 at both ends, ±amplitude at each knot.
      const t = (x / (L || 1)) * n;
      const i = Math.floor(t);
      const fr = t - i;
      const at = (j) => (j <= 0 || j >= n ? 0 : j % 2 ? -W.zig : W.zig);
      return at(i) + (at(i + 1) - at(i)) * fr;
    };
    return [{ c, hw: () => W.zigBand / 2, knots, from: 0, to: L }];
  }
  if (kind === 'duplicates') {
    const hw = () => W.double / 2;
    return [
      { c: () => -W.doubleGap, hw, knots: [], from: 0, to: L },
      { c: () => W.doubleGap, hw, knots: [], from: 0, to: L },
    ];
  }
  if (DIRECTED[kind]) {
    const m = kind === 'supersedes' ? W.heavy : 1;
    const h = lin((W.source * m) / 2, (W.target * m) / 2);
    if (kind === 'derived_from') {
      // A ribbed wedge: a thin spine with cross ribs as tall as the wedge,
      // so it never relies on telling blue from green or violet.
      const out = [{ c: zero, hw: () => W.spine / 2, knots: [], from: 0, to: L }];
      const step = A.rib * k;
      const rib = A.ribW * k;
      for (let x = 0; x + rib <= L; x += step) out.push({ c: zero, hw: h, knots: [], from: x, to: x + rib });
      return out;
    }
    return [{ c: zero, hw: h, knots: [], from: 0, to: L }];
  }
  // related_to, and any kind a newer server sends: thin and symmetric.
  return [{ c: zero, hw: () => W.thin / 2, knots: [], from: 0, to: L }];
}

/**
 * The path data of one relation of length `L` in the local frame (module
 * docs). `k`: layout units per screen px along the relation, at the zoom it
 * is drawn at (1 for a swatch). `end`: 'bar' adds a dangling stub's bar.
 */
export function relationPath(kind, status, L, k = 1, end = null) {
  if (!(L > 0.5)) return '';
  const directed = !!DIRECTED[kind];
  // A proposed directed relation: one chevron at the middle, pointing at
  // the target, with the dashes kept clear of it.
  const chev = status === 'proposed' && directed && L > 3 * A.chevDepth * k;
  const mid = L / 2;
  const depth = A.chevDepth * k;
  const hole = chev ? [mid - depth / 2 - A.chevClear * k, mid + depth / 2 + A.chevClear * k] : null;
  const parts = [];
  const teeth = kind === 'contradicts' ? L / zigCount(L, k) : 0;
  for (const [a, b] of intervals(status, L, k, hole, teeth)) {
    for (const bd of bandsOf(kind, L, k)) {
      const lo = Math.max(a, bd.from);
      const hi = Math.min(b, bd.to);
      if (hi - lo > 0.05) parts.push(band(lo, hi, bd.c, bd.hw, bd.knots));
    }
  }
  if (chev) {
    const x0 = mid - depth / 2;
    const x1 = mid + depth / 2;
    const t = A.chevThick * k;
    const hh = W.chev * (kind === 'supersedes' ? W.heavy : 1);
    parts.push(`M${f1(x0)},${f2(-hh)}L${f1(x0 + t)},${f2(-hh)}L${f1(x1 + t)},0L${f1(x0 + t)},${f2(hh)}L${f1(x0)},${f2(hh)}L${f1(x1)},0Z`);
  }
  if (end === 'bar') {
    const b = A.bar * k;
    parts.push(`M${f1(L - b)},${f2(-W.bar)}L${f1(L)},${f2(-W.bar)}L${f1(L)},${f2(W.bar)}L${f1(L - b)},${f2(W.bar)}Z`);
  }
  return parts.join('');
}

/**
 * One graph relation along `pts` (Plain's routes are straight; a polyline
 * is drawn source to last point). `ends`: the core's { rs, rt, stub, scale }.
 * Returns the `<path>`, placed by a CSS transform (module docs), or null
 * when the two memories touch.
 */
export function drawRelation(e, pts, ends) {
  const [ax, ay] = pts[0];
  const [bx, by] = pts[pts.length - 1];
  const dx = bx - ax;
  const dy = by - ay;
  const len = Math.sqrt(dx * dx + dy * dy);
  if (!(len > 0)) return null;
  const rs = Math.min(ends.rs, len / 2);
  const rt = ends.stub ? 0 : Math.min(ends.rt, len / 2);
  const L = len - rs - rt;
  const k = zoomFactor(ends.scale || 1);
  const d = relationPath(e.kind, e.status, L, k, ends.stub && e.state !== 'unknown' ? 'bar' : null);
  if (!d) return null;
  const ux = dx / len;
  const uy = dy / len;
  const el = s('path', { class: ['rel', ends.stub && 'stub'], d, mask: ends.stub ? `url(#${FADE_ID})` : null });
  const deg = (Math.atan2(dy, dx) * 180) / Math.PI;
  // A CSSOM write, not an attribute: the CSP has no 'unsafe-inline'.
  el.style.transform = `translate(${f1(ax + ux * rs)}px,${f1(ay + uy * rs)}px) rotate(${f2(deg)}deg) scale(1,calc(var(--zw,1) * var(--zk,1)))`;
  return el;
}

/** The id of the graph's stub fade (defined once, by `fadeDefs`). */
export const FADE_ID = 'pl-fade';

let fadeSeq = 0;

/**
 * A `<defs>` holding a mask that fades its element along its own x (the
 * local frame runs source → far end): opaque at the source, faint at the
 * end. `id` names the mask; the gradient gets `id-g`.
 */
export function fadeDefs(id = FADE_ID) {
  return s(
    'defs',
    null,
    s(
      'linearGradient',
      { id: id + '-g', x1: 0, x2: 1, y1: 0, y2: 0 },
      s('stop', { offset: 0, 'stop-color': '#fff', 'stop-opacity': 1 }),
      s('stop', { offset: 0.35, 'stop-color': '#fff', 'stop-opacity': 0.9 }),
      s('stop', { offset: 1, 'stop-color': '#fff', 'stop-opacity': 0.12 }),
    ),
    s('mask', { id, maskContentUnits: 'objectBoundingBox' }, s('rect', { x: 0, y: -0.5, width: 1, height: 2, fill: `url(#${id}-g)` })),
  );
}

/** A small sample of a relation (legends, tables, lists), from `relationPath`. */
export function relationSample(kind, status, width = 44) {
  const L = width - 8;
  return s(
    'svg',
    { class: [...edgeClass(kind, status), 'pl-sample'], width, height: 16, viewBox: `0 0 ${width} 16`, 'aria-hidden': 'true' },
    s('path', { class: 'rel', d: relationPath(kind, status, L), transform: 'translate(4,8)' }),
  );
}

/** A legend sample of a stub: a `dangling` (deleted) or `unknown` far end. */
export function stubSwatch(state) {
  const id = `pl-fade-s${++fadeSeq}`;
  return s(
    'svg',
    { class: [...edgeClass('supports', 'confirmed', state), 'pl-sample'], width: 44, height: 16, viewBox: '0 0 44 16', 'aria-hidden': 'true' },
    fadeDefs(id),
    s('path', { class: 'rel stub', d: relationPath('supports', 'confirmed', 36, 1, state === 'unknown' ? null : 'bar'), transform: 'translate(4,8)', mask: `url(#${id})` }),
  );
}
