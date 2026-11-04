// The Plain style's relation drawing, without arrowheads. One function,
// `relationPath`, builds every relation's geometry: the graph's relations
// (`drawRelation`), the legend's and every other page's samples
// (`relationSample`, `stubSwatch`) all call it, so a legend can never drift
// from the graph. bench/relations.mjs checks its geometry (no NaN, wide end
// at the source, chevron direction, bounded path data, unknown kinds).
//
// Visual language (colours are tokens in css/themes/plain.css):
//   direction     a tapered wedge, wide at the source (the memory that
//                 supports, supersedes or is derived from the other; about
//                 3.3× the target width), narrowing to the target; symmetric
//                 kinds are untapered. A directed relation shorter than
//                 `SHORT_PX` on screen also gets the mid chevron, since a
//                 taper over a few pixels reads as a tick.
//   supports      green solid wedge
//   derived_from  blue comb: a solid narrower wedge with cross ribs standing
//                 out of it, so it is never broken (dashed = proposed) and
//                 never relies on telling blue from green or violet
//   supersedes    the heaviest wedge, in the text colour
//   contradicts   red zigzag
//   duplicates    violet double stroke
//   related_to    thin grey stroke
//   proposed      dashed, with one mid-relation chevron pointing source →
//                 target (directed kinds only; symmetric ones are dashed only)
//   rejected      faint and dotted (drawn only with the Rejected filter on)
//   stub          the same drawing fading toward the far end; a relation to
//                 a deleted memory (`state: dangling`) fades less and ends in
//                 a short bar; any other far end not drawn here (a store not
//                 checked, or a live memory in another store) just fades out
// Every kind differs by shape as well as colour (wedge / comb / heavy wedge /
// zigzag / double / thin), so colour is never the only cue.
//
// **Geometry.** Everything is filled bands in a local frame: x runs along
// the relation from the source's edge (0) to the target's edge (L), y is
// across it, in screen pixels at scale 1 (written as integers of 1/`UNIT`
// px, which the transform scales back). A relation is ONE `<path>`: its
// dashes, chevron and end bar are subpaths of the same path data. On the
// graph the path is placed by a CSS transform, `translate rotate
// scale(1/UNIT, 1/UNIT × var(--zw) × var(--zk))`, where `--zw` is set once per settled zoom on the
// `<svg>` (see `zoomFactor`), so a zoom rescales every relation's width with
// no per-relation work in JS, and `--zk` thickens a highlighted one. Lengths
// along the relation (dash, zigzag period, ribs, chevron) are sized for the
// zoom they were drawn at (`k`); once a zoom settles more than
// `REDRAW_RATIO` away from it, the style asks the core to redraw the
// relations (`redrawOnZoom`), so they stay at a sensible screen size over
// the whole zoom range.
//
// **Cost** is linear in the detail drawn and bounded per relation: knots
// and ribs are computed per dash interval by index, never filtered, and the
// along-relation unit is raised so no relation carries more than
// `MAX_DETAIL_PX` of detail (dashes, dots, ribs and teeth all bounded). A
// very long relation therefore gets coarser dashes, never megabytes of path.

import { s } from '../../../dom.js';
import { isDirected, edgeClass } from '../../../relations.js';

/** Widths across a relation, in screen px at scale 1 (before `--zw`). */
const W = {
  target: 1.5, // a wedge's narrow end
  source: 5, // its wide end
  heavy: 1.45, // supersedes: every wedge width × this
  comb: 0.5, // derived_from: its solid core, × the wedge's half-width
  rib: 0.6, // … and how far its ribs stand out of the wedge
  thin: 1.25, // related_to
  double: 1.0, // each of duplicates' two strokes
  doubleGap: 1.7, // their centres sit at ± this
  zig: 2.6, // contradicts' amplitude
  zigBand: 2.1, // its vertical thickness
  chev: 4.2, // the chevron's half-height
  bar: 5, // a dangling stub's end bar, half-height
};

/** Lengths along a relation, in screen px at the zoom it is drawn at. */
const A = { dash: 6, gap: 4, dot: 1.6, dotGap: 3.4, zigHalf: 3.4, chevDepth: 4.2, chevThick: 1.8, chevClear: 3, rib: 2.8, ribW: 1.2, bar: 2.2 };

/**
 * At most this many screen px of along-relation detail per relation, at
 * the unit it is drawn at: past it the unit grows, so a longer relation gets
 * proportionally longer dashes, ribs and teeth (≤ 53 zigzag half-periods,
 * ≤ 64 ribs, ≤ 18 dashes), never more of them.
 */
const MAX_DETAIL_PX = 180;

/** A confirmed directed relation shorter than this on screen gets the mid chevron. */
export const SHORT_PX = 40;

/** Redraw once the zoom settles this far (as a ratio of `zoomFactor`) from the drawn one. */
export const REDRAW_RATIO = 1.5;

/**
 * `--zw` for a zoom `scale` (screen px per layout unit): widths grow as
 * scale^0.4 on screen, so a relation stays legible zoomed out and thickens a
 * little zoomed in, instead of following the map 1:1. Also the
 * along-relation unit `k` at that zoom.
 */
export function zoomFactor(scale) {
  const z = Math.pow(Math.max(scale, 1e-3), -0.6);
  return Math.min(8, Math.max(0.25, z));
}

/** Whether relations drawn at `drawnScale` should be redrawn at `scale`. */
export function redrawOnZoom(drawnScale, scale) {
  const r = zoomFactor(drawnScale) / zoomFactor(scale);
  return r > REDRAW_RATIO || r < 1 / REDRAW_RATIO;
}

/**
 * Path data is written in integer units of 1/`UNIT` px (and scaled back by
 * the transform): formatting integers is several times cheaper than
 * formatting decimals, which dominated building a zoomed-in graph.
 */
export const UNIT = 20;
const q = (v) => '' + Math.round(v * UNIT);
const f1 = (v) => (Math.round(v * 10) / 10).toString();
const f2 = (v) => (Math.round(v * 100) / 100).toString();

/**
 * The dash intervals [[a, b], …] along [0, L] for a status, less `hole`
 * (the chevron's stretch). `teeth`: a zigzag's knot spacing, so a dashed
 * zigzag breaks between whole teeth and still reads as one.
 */
function intervals(status, L, k, hole, teeth) {
  let on;
  let off;
  if (status === 'proposed' && teeth) [on, off] = [3 * teeth, teeth];
  else if (status === 'proposed') [on, off] = [A.dash * k, A.gap * k];
  else if (status === 'rejected') [on, off] = [A.dot * k, A.dotGap * k];
  else return hole ? [[0, hole[0]], [hole[1], L]] : [[0, L]];
  const period = on + off;
  const n = Math.max(1, Math.round((L + off) / period));
  // Stretch the pattern so both ends get a whole dash (a zigzag's already fits).
  const scaleTo = teeth ? 1 : (L + off) / (n * period);
  const out = [];
  for (let i = 0; i < n; i++) {
    const a = i * period * scaleTo;
    const b = Math.min(L, a + on * scaleTo);
    if (!hole || b <= hole[0] || a >= hole[1]) out.push([a, b]);
    else {
      if (a < hole[0]) out.push([a, hole[0]]);
      if (b > hole[1]) out.push([hole[1], b]);
    }
  }
  return out;
}

/**
 * A band's polygon over [a, b]: `c(x)` its centre, `hw(x)` its half-width,
 * `knots(a, b)` the x of every bend of the centre strictly inside (a, b),
 * all in the local frame. Linear between knots, so the polygon is exact.
 */
function band(a, b, c, hw, knots) {
  const xs = knots ? [a, ...knots(a, b), b] : [a, b];
  let top = '';
  let bot = '';
  for (let i = 0; i < xs.length; i++) {
    const x = xs[i];
    top += (i ? 'L' : 'M') + q(x) + ',' + q(c(x) - hw(x));
    const y = xs[xs.length - 1 - i];
    bot += 'L' + q(y) + ',' + q(c(y) + hw(y));
  }
  return top + bot + 'Z';
}

/**
 * The bands that draw `kind` over length L: [{ c, hw, knots, from, to }],
 * or with `ribs: { step, w }` a band repeated every `step` for `w`. `k`:
 * the along-relation unit (zigzag period, rib spacing).
 */
function bandsOf(kind, L, k) {
  const zero = () => 0;
  if (kind === 'contradicts') {
    const n = zigCount(L, k);
    const step = L / n;
    // Knot j sits at j·step, ±amplitude alternately; the ends are on the axis.
    const at = (j) => (j <= 0 || j >= n ? 0 : j % 2 ? -W.zig : W.zig);
    const c = (x) => {
      const t = x / step;
      const j = Math.min(n - 1, Math.floor(t));
      return at(j) + (at(j + 1) - at(j)) * (t - j);
    };
    const knots = (a, b) => {
      const out = [];
      for (let j = Math.floor(a / step) + 1; j * step < b && j < n; j++) if (j * step > a) out.push(j * step);
      return out;
    };
    return [{ c, hw: () => W.zigBand / 2, knots, from: 0, to: L }];
  }
  if (kind === 'duplicates') {
    const hw = () => W.double / 2;
    return [
      { c: () => -W.doubleGap, hw, from: 0, to: L },
      { c: () => W.doubleGap, hw, from: 0, to: L },
    ];
  }
  if (isDirected(kind)) {
    const m = kind === 'supersedes' ? W.heavy : 1;
    const h0 = (W.source * m) / 2;
    const h1 = (W.target * m) / 2;
    const h = (x) => h0 + ((h1 - h0) * x) / (L || 1);
    if (kind === 'derived_from') {
      return [
        { c: zero, hw: (x) => Math.max(0.55, h(x) * W.comb), from: 0, to: L },
        { c: zero, hw: (x) => h(x) + W.rib, from: 0, to: L, ribs: { step: A.rib * k, w: A.ribW * k } },
      ];
    }
    return [{ c: zero, hw: h, from: 0, to: L }];
  }
  // related_to, and any kind a newer server sends: thin and symmetric.
  return [{ c: zero, hw: () => W.thin / 2, from: 0, to: L }];
}

/** How many half-periods a zigzag of length L has at unit `k`. */
const zigCount = (L, k) => Math.max(2, Math.round(L / (A.zigHalf * k)));

/**
 * The path data of one relation of length `L` in the local frame (module
 * docs). `k`: layout units per screen px along the relation, at the zoom it
 * is drawn at (1 for a swatch). `opts.end`: 'bar' adds a dangling stub's
 * bar; `opts.chevron`: give a confirmed directed relation the mid chevron.
 */
export function relationPath(kind, status, L, k = 1, opts = {}) {
  if (!(L > 0.5) || !(k > 0)) return '';
  // Bound the detail (module docs, Cost).
  k = Math.max(k, L / MAX_DETAIL_PX);
  const directed = isDirected(kind);
  const depth = A.chevDepth * k;
  const chev = directed && (status === 'proposed' || (opts.chevron && status !== 'rejected')) && L > 3 * depth;
  const mid = L / 2;
  const hole = chev ? [mid - depth / 2 - A.chevClear * k, mid + depth / 2 + A.chevClear * k] : null;
  const bands = bandsOf(kind, L, k);
  const teeth = kind === 'contradicts' ? L / zigCount(L, k) : 0;
  let d = '';
  for (const [a, b] of intervals(status, L, k, hole, teeth)) {
    for (const bd of bands) {
      const lo = Math.max(a, bd.from);
      const hi = Math.min(b, bd.to);
      if (!(hi - lo > 0.05)) continue;
      if (!bd.ribs) {
        d += band(lo, hi, bd.c, bd.hw, bd.knots);
        continue;
      }
      // Only the ribs that meet [lo, hi], by index.
      const { step, w } = bd.ribs;
      for (let j = Math.max(0, Math.ceil((lo - w) / step)); j * step < hi; j++) {
        const r0 = Math.max(lo, j * step);
        const r1 = Math.min(hi, j * step + w);
        if (r1 - r0 > 0.05) d += band(r0, r1, bd.c, bd.hw);
      }
    }
  }
  if (chev) {
    const x0 = mid - depth / 2;
    const x1 = mid + depth / 2;
    const t = A.chevThick * k;
    const hh = W.chev * (kind === 'supersedes' ? W.heavy : 1);
    d += `M${q(x0)},${q(-hh)}L${q(x0 + t)},${q(-hh)}L${q(x1 + t)},0L${q(x0 + t)},${q(hh)}L${q(x0)},${q(hh)}L${q(x1)},0Z`;
  }
  if (opts.end === 'bar') {
    const b = Math.min(L / 2, A.bar * k);
    d += `M${q(L - b)},${q(-W.bar)}L${q(L)},${q(-W.bar)}L${q(L)},${q(W.bar)}L${q(L - b)},${q(W.bar)}Z`;
  }
  return d;
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
  const scale = ends.scale || 1;
  const dangling = ends.stub && e.state === 'dangling';
  const d = relationPath(e.kind, e.status, L, zoomFactor(scale), { end: dangling ? 'bar' : null, chevron: L * scale < SHORT_PX });
  if (!d) return null;
  const ux = dx / len;
  const uy = dy / len;
  const mask = ends.stub ? `url(#${dangling ? FADE_ID + '-d' : FADE_ID})` : null;
  const el = s('path', { class: ['rel', ends.stub && 'stub'], d, mask });
  const deg = (Math.atan2(dy, dx) * 180) / Math.PI;
  // A CSSOM write, not an attribute: the CSP has no 'unsafe-inline'.
  el.style.transform = `translate(${f1(ax + ux * rs)}px,${f1(ay + uy * rs)}px) rotate(${f2(deg)}deg) scale(${1 / UNIT},calc(${1 / UNIT} * var(--zw,1) * var(--zk,1)))`;
  return el;
}

/** The id of the graph's stub fade; `-d` is the dangling one (defined once, by `fadeDefs`). */
export const FADE_ID = 'pl-fade';

/**
 * A mask fading its element along its own x (the local frame runs source →
 * far end), opaque at the source and `last` at the far end. `id` names the
 * mask; its gradient gets `id-g`.
 */
function fadeMask(id, last) {
  return [
    s(
      'linearGradient',
      { id: id + '-g', x1: 0, x2: 1, y1: 0, y2: 0 },
      s('stop', { offset: 0, 'stop-color': '#fff', 'stop-opacity': 1 }),
      s('stop', { offset: 0.35, 'stop-color': '#fff', 'stop-opacity': 0.9 }),
      s('stop', { offset: 1, 'stop-color': '#fff', 'stop-opacity': last }),
    ),
    s('mask', { id, maskContentUnits: 'objectBoundingBox' }, s('rect', { x: 0, y: -0.5, width: 1, height: 2, fill: `url(#${id}-g)` })),
  ];
}

/** How faint a stub gets at its far end: a dangling one stays at 0.6, so its end bar reads. */
const FADE_TO = 0.12;
const FADE_TO_DANGLING = 0.6;

/** The graph's `<defs>` for both stub fades. */
export function fadeDefs() {
  return s('defs', null, fadeMask(FADE_ID, FADE_TO), fadeMask(FADE_ID + '-d', FADE_TO_DANGLING));
}

let fadeSeq = 0;

/** A small sample of a relation (legends, tables, lists), from `relationPath`. */
export function relationSample(kind, status, width = 44) {
  const L = width - 8;
  return s(
    'svg',
    { class: [...edgeClass(kind, status), 'pl-sample'], width, height: 16, viewBox: `0 0 ${width} 16`, 'aria-hidden': 'true' },
    s('path', { class: 'rel', d: relationPath(kind, status, L), transform: `translate(4,8) scale(${1 / UNIT})` }),
  );
}

/** A legend sample of a stub: a `dangling` (deleted) or `unknown` far end. */
export function stubSwatch(state) {
  const id = `pl-fade-s${++fadeSeq}`;
  const dangling = state === 'dangling';
  return s(
    'svg',
    { class: [...edgeClass('supports', 'confirmed', state), 'pl-sample'], width: 44, height: 16, viewBox: '0 0 44 16', 'aria-hidden': 'true' },
    s('defs', null, fadeMask(id, dangling ? FADE_TO_DANGLING : FADE_TO)),
    s('path', { class: 'rel stub', d: relationPath('supports', 'confirmed', 36, 1, { end: dangling ? 'bar' : null }), transform: `translate(4,8) scale(${1 / UNIT})`, mask: `url(#${id})` }),
  );
}
