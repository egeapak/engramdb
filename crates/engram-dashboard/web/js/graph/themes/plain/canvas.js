// Plain on a canvas: the draw hooks the core canvas host (graph/canvas/host.js)
// calls when Plain is picked. Same geometry as the SVG hooks in ./index.js,
// from the same functions: a relation's shape is `relationShape` (the
// SVG's `relationPath`, written straight into a Path2D at the same integer
// coordinates, placed by the same matrix the SVG path's CSS transform writes), a
// region is `regionDiscs`' discs (the SVG's `regionArcs`), a memory's shape follows
// scopes.js `typeShape`. Colours are the tokens of css/themes/plain.css,
// read once per light/dark switch by the host (`tokens`).
//
// **Batching.** Relations are filled once per (kind, rejected): every
// relation of a group is written into one Path2D, placed by its own matrix
// (`placedSink`), and the merge is kept until the width factor (`zoomFactor`, the SVG's
// `--zw`) moves 2 % or the relation set changes, so a settle that only moved
// the view re-fills ~12 paths. Memories likewise, one Path2D per look
// (fill, stroke, width, dash). A stub (a relation whose far end is not
// drawn) has its own fading gradient, so it is filled on its own; there are
// few. Filling by group changes which relation is drawn over which where
// two cross (SVG draws them in index order): within a group a crossing is
// one colour either way, and groups are filled faint-first
// (`KIND_ORDER`), so a contradiction is never under a related_to.

import { slot } from '../../../scopes.js';
import { reads } from '../../radius.js';
import { regionDiscs, texture } from './regions.js';
import { relationShape, path2dSink, UNIT, SHORT_PX, zoomFactor, FADE_TO, FADE_TO_DANGLING } from './relations.js';

/**
 * What plain.css says about highlighting, as numbers the host applies: the
 * faded opacity of memories, relations, labels and the backdrop under a
 * focus (Plain's backdrop does not fade), how much wider a lit relation is
 * (`--zk`), the relation click target's width (`.cx .hit`, screen px) and
 * the lit labels' weights.
 */
const look = Object.freeze({ fadeNode: 0.14, fadeEdge: 0.14, fadeLabel: 0.24, fadeBackdrop: 1, litWidth: 1.5, hitWidth: 12, labelWeight: Object.freeze({ focus: 700, nb: 500 }) });

const KINDS = ['contradicts', 'supersedes', 'supports', 'derived_from', 'duplicates', 'related_to'];

/** Fill order of relation groups: faint and common first, so the strongest cue is on top. */
const KIND_ORDER = ['related_to', 'duplicates', 'derived_from', 'supports', 'supersedes', 'contradicts'];

/** The region / texture opacities and the name look, from plain.css. */
const TEX_ALPHA = 0.28;
const TEX_WIDTH = 1.6;
/** The size scope names are set at, then scaled to theirs. */
const NAME_EM = 100;
const NAME_ALPHA = 0.8;
const NAME_BG_ALPHA = 0.72;
const REJECTED_ALPHA = 0.55;

/** Plain's colour tokens (`read(name)`: a custom property's computed value). */
function tokens(read) {
  const kinds = {};
  for (const k of KINDS) kinds[k] = read(`--pl-${k}`);
  return { kinds, node: read('--pl-node'), regionA: parseFloat(read('--pl-region-a')) || 0.065, slots: [0, 1, 2, 3, 4, 5, 6].map((i) => read(`--scope-${i}`)), none: read('--scope-none') };
}

const slotColour = (T, c) => (c === 'none' ? T.none : T.slots[+c] || T.none);

/**
 * A region's discs as one Path2D, every disc the same way round so the
 * union fills once (nonzero), as the SVG's arcs do. Built with `arc`
 * rather than from `regionArcs`' path data: parsing that string cost
 * 1–2 ms on a 2k graph for the same shape.
 */
function discPath(discs) {
  const p = new Path2D();
  for (let j = 0; j < discs.length; j += 3) {
    p.moveTo(discs[j] + discs[j + 2], discs[j + 1]);
    p.arc(discs[j], discs[j + 1], discs[j + 2], 0, 2 * Math.PI);
  }
  return p;
}

/** What the backdrop draws, prepared once per graph: regions (path, tint, texture) and the placed names. */
function backdrop(g, lay) {
  const regions = regionDiscs(g, lay).map((a) => ({ path: discPath(a.discs), c: slot(a.slot), v: texture(a.slot) }));
  const names = (lay.scopes || []).filter((nm) => nm.placed);
  return { regions, names, patterns: new Map() };
}

/**
 * A region texture as a CanvasPattern: the SVG pattern's 9-unit tile
 * (`regionPattern` in ./index.js: a stripe on the tile's edge, so only its
 * inner half shows, or a dot), drawn at `px` device pixels per layout unit
 * and mapped back to layout units by the pattern's own transform, rotated
 * 45° as the SVG's `patternTransform` is.
 */
function pattern(ctx, bd, T, v, c, px) {
  const n = Math.max(3, Math.min(256, Math.round(9 * px)));
  // Keyed by the colour too, so a light/dark switch needs no clearing.
  const colour = slotColour(T, c);
  const key = `${v}|${colour}|${n}`;
  let p = bd.patterns.get(key);
  if (p) return p;
  const tile = document.createElement('canvas');
  tile.width = n;
  tile.height = n;
  const tc = tile.getContext('2d');
  tc.scale(n / 9, n / 9);
  tc.globalAlpha = TEX_ALPHA;
  if (v === 2) {
    tc.fillStyle = colour;
    tc.beginPath();
    tc.arc(4.5, 4.5, 1.5, 0, 2 * Math.PI);
    tc.fill();
  } else {
    tc.strokeStyle = colour;
    tc.lineWidth = TEX_WIDTH;
    tc.beginPath();
    tc.moveTo(0, 0);
    tc.lineTo(0, 9);
    if (v === 3) {
      tc.moveTo(0, 0);
      tc.lineTo(9, 0);
    }
    tc.stroke();
  }
  p = ctx.createPattern(tile, 'repeat');
  const m = new DOMMatrix();
  if (v !== 2) m.rotateSelf(45);
  m.scaleSelf(9 / n, 9 / n);
  p.setTransform(m);
  bd.patterns.set(key, p);
  return p;
}

/** Under the relations: each region's tint, then its texture past the seventh scope. */
function drawBelow(ctx, bd, T, fr) {
  for (const r of bd.regions) {
    ctx.globalAlpha = T.regionA;
    ctx.fillStyle = slotColour(T, r.c);
    ctx.fill(r.path);
    if (r.v) {
      ctx.globalAlpha = 1;
      ctx.fillStyle = pattern(ctx, bd, T, r.v, r.c, fr.px);
      ctx.fill(r.path);
    }
  }
  ctx.globalAlpha = 1;
}

/**
 * Over the relations: one name per scope (and its faint backing box when it
 * covers memories). One font string for every name, sized by the transform:
 * each new `ctx.font` string is parsed and resolved again, and on a canvas
 * in the document that resolution forces a style recalculation.
 */
function drawAbove(ctx, bd, T, fr) {
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  ctx.lineJoin = 'round';
  ctx.font = `700 ${NAME_EM}px ${fr.font}`;
  ctx.letterSpacing = `${0.02 * NAME_EM}px`;
  ctx.lineWidth = 0.25 * NAME_EM;
  ctx.strokeStyle = T.bg;
  for (const nm of bd.names) {
    if (nm.backed) {
      const w = nm.w || nm.key.length * nm.size * 0.62;
      ctx.globalAlpha = NAME_BG_ALPHA;
      ctx.fillStyle = T.bg;
      ctx.beginPath();
      ctx.roundRect(nm.x - w / 2, nm.y - nm.size * 0.96, w, nm.size * 1.2, nm.size * 0.3);
      ctx.fill();
    }
    const k = nm.size / NAME_EM;
    ctx.save();
    ctx.transform(k, 0, 0, k, nm.x, nm.y);
    ctx.globalAlpha = 1;
    ctx.strokeText(nm.key, 0, 0);
    ctx.globalAlpha = NAME_ALPHA;
    ctx.fillStyle = T.ink2;
    ctx.fillText(nm.key, 0, 0);
    ctx.restore();
  }
  ctx.letterSpacing = '0px';
  ctx.globalAlpha = 1;
}

/**
 * One relation, for the host to keep: what its shape is in the local frame
 * (the SVG's `drawRelation`, same numbers: `relationShape`'s arguments) and
 * where that frame sits, or null when the two memories touch. The shape
 * itself is written when the relation is drawn (`groupsOf`). `ends`: the
 * core's { rs, rt, stub, scale }.
 */
function edge(e, pts, ends) {
  const [ax, ay] = pts[0];
  const [bx, by] = pts[pts.length - 1];
  const dx = bx - ax;
  const dy = by - ay;
  const len = Math.sqrt(dx * dx + dy * dy);
  if (!(len > 0)) return null;
  const rs = Math.min(ends.rs, len / 2);
  const rt = ends.stub ? 0 : Math.min(ends.rt, len / 2);
  const L = len - rs - rt;
  // relationShape draws nothing this short (as relationPath gives '').
  if (!(L > 0.5)) return null;
  const scale = ends.scale || 1;
  const dangling = !!ends.stub && e.state === 'dangling';
  const ux = dx / len;
  const uy = dy / len;
  return {
    // A kind a newer server sends is drawn as related_to, as the CSS's default fill does.
    kind: KINDS.includes(e.kind) ? e.kind : 'related_to',
    rejected: e.status === 'rejected',
    stub: !!ends.stub,
    dangling,
    L,
    shape: [e.kind, e.status, L, zoomFactor(scale), { end: dangling ? 'bar' : null, chevron: L * scale < SHORT_PX }],
    tx: ax + ux * rs,
    ty: ay + uy * rs,
    ux,
    uy,
  };
}

/**
 * A sink (relations.js) that writes a relation's shape into `p` already
 * placed: the local frame's matrix (translate, rotate, scale(1/UNIT,
 * z/UNIT), as the SVG's CSS transform) applied to the same integer
 * coordinates the SVG's path data holds. Writing every relation of a group
 * straight into the group's path costs a third of building a Path2D per
 * relation and merging them with `addPath` (measured).
 */
function placedSink(p, r, z) {
  const a = r.ux / UNIT;
  const b = r.uy / UNIT;
  const c = (-r.uy * z) / UNIT;
  const d = (r.ux * z) / UNIT;
  const e = r.tx;
  const f = r.ty;
  return {
    m(x, y) {
      const X = Math.round(x * UNIT);
      const Y = Math.round(y * UNIT);
      p.moveTo(a * X + c * Y + e, b * X + d * Y + f);
    },
    l(x, y) {
      const X = Math.round(x * UNIT);
      const Y = Math.round(y * UNIT);
      p.lineTo(a * X + c * Y + e, b * X + d * Y + f);
    },
    // No closePath: a fill closes every subpath itself, and Chrome's
    // Path2D.closePath costs time in proportion to the path so far, so a
    // group's closes were quadratic (10k points with 667 closes: 10 ms,
    // without them 0.7 ms).
    z() {},
  };
}

/** A stub's shape in its own local frame (filled under a transform, for its gradient). */
function localPath(r) {
  if (!r.path) {
    r.path = new Path2D();
    relationShape(path2dSink(r.path), ...r.shape);
  }
  return r.path;
}

/** Merged relation groups per relation list: the merge is reused while its width factor holds (module docs). */
const merged = new WeakMap();

function groupsOf(recs, z) {
  const had = merged.get(recs);
  if (had && Math.abs(z - had.z) / had.z < 0.02) return had;
  const groups = new Map();
  const stubs = [];
  for (const r of recs) {
    if (r.stub) {
      stubs.push(r);
      continue;
    }
    const key = r.kind + (r.rejected ? '|r' : '');
    let p = groups.get(key);
    if (!p) groups.set(key, (p = new Path2D()));
    relationShape(placedSink(p, r, z), ...r.shape);
  }
  const order = [];
  for (const rej of [true, false]) for (const k of KIND_ORDER) if (groups.has(k + (rej ? '|r' : ''))) order.push({ kind: k, rejected: rej, path: groups.get(k + (rej ? '|r' : '')) });
  const out = { z, order, stubs };
  merged.set(recs, out);
  return out;
}

/**
 * Run the relation drawing over made-up relations while the page is idle,
 * so the first graph finds it compiled. Cold, writing a 2k graph's 610
 * relations cost 9.3 ms of the first paint; the same calls once the engine
 * has optimised them cost under 1 ms (docs/contributors/dashboard-performance.md).
 * The same sink and Path2D calls as `groupsOf`, so the code it optimises is
 * the code the graph runs; in slices that yield to input, and never again
 * once done. Nothing it builds is kept.
 */
const WARM_CALLS = 1800;
let warmed = 0;
function warmStep(deadline) {
  const statuses = ['confirmed', 'confirmed', 'proposed', 'rejected'];
  const r = { ux: 0.8, uy: 0.6, tx: 10, ty: 20 };
  while (warmed < WARM_CALLS && (!deadline || deadline.timeRemaining() > 1)) {
    const p = new Path2D();
    for (let j = 0; j < 60 && warmed < WARM_CALLS; j++, warmed++) {
      const i = warmed;
      relationShape(placedSink(p, r, 1), KINDS[i % 6], statuses[(i >> 1) % 4], 24 + ((i * 37) % 160), 1, { end: i % 23 === 0 ? 'bar' : null, chevron: i % 7 === 0 });
    }
  }
  if (warmed < WARM_CALLS) requestIdleCallback(warmStep, { timeout: 1000 });
}
if (typeof requestIdleCallback === 'function' && typeof Path2D === 'function') requestIdleCallback(warmStep, { timeout: 1000 });

/**
 * Where relation `rec` lies in the drawing order (`drawEdges`: rejected
 * groups first, then by `KIND_ORDER`, stubs last), so the host's hit test
 * picks the one drawn on top.
 */
function edgeRank(rec) {
  return rec.stub ? 2 * KIND_ORDER.length : (rec.rejected ? 0 : KIND_ORDER.length) + KIND_ORDER.indexOf(rec.kind);
}

const rgba = (hex, a) => {
  const m = /^#?([0-9a-f]{6})$/i.exec(String(hex).trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  return `rgba(${n >> 16},${(n >> 8) & 255},${n & 255},${a})`;
};

/**
 * Relations `recs` (from `edge`), batched (module docs). `o`: { scale, lit
 * (drawn lit: `look.litWidth` wider) }. A stub fades along its own
 * frame exactly as the SVG mask does (opaque at the source, 0.9 at 35 %,
 * `FADE_TO` at the far end; a dangling one stays at `FADE_TO_DANGLING`).
 */
function drawEdges(ctx, recs, T, o) {
  if (!recs.length) return;
  const z = zoomFactor(o.scale) * (o.lit ? look.litWidth : 1);
  const { order, stubs } = groupsOf(recs, z);
  for (const gp of order) {
    ctx.globalAlpha = gp.rejected ? REJECTED_ALPHA : 1;
    ctx.fillStyle = T.kinds[gp.kind];
    ctx.fill(gp.path);
  }
  for (const r of stubs) {
    const colour = T.kinds[r.kind];
    const last = r.dangling ? FADE_TO_DANGLING : FADE_TO;
    ctx.save();
    ctx.transform(r.ux / UNIT, r.uy / UNIT, (-r.uy * z) / UNIT, (r.ux * z) / UNIT, r.tx, r.ty);
    const gr = ctx.createLinearGradient(0, 0, r.L * UNIT, 0);
    gr.addColorStop(0, rgba(colour, 1));
    gr.addColorStop(0.35, rgba(colour, 0.9));
    gr.addColorStop(1, rgba(colour, last));
    ctx.globalAlpha = r.rejected ? REJECTED_ALPHA : 1;
    ctx.fillStyle = gr;
    ctx.fill(localPath(r));
    ctx.restore();
  }
  ctx.globalAlpha = 1;
}

/** A memory's shape (scopes.js `typeShape`), centred on (x, y), into `p`. */
function shape(p, type, x, y, r) {
  switch (type) {
    case 'convention':
    case 'preference': {
      const q = r * 0.92;
      p.roundRect(x - q, y - q, 2 * q, 2 * q, r * (type === 'preference' ? 0.6 : 0.28));
      return;
    }
    case 'hazard': {
      const k = r * 1.25;
      p.moveTo(x, y - k);
      p.lineTo(x + k, y);
      p.lineTo(x, y + k);
      p.lineTo(x - k, y);
      p.closePath();
      return;
    }
    case 'debug': {
      const k = r * 1.2;
      p.moveTo(x, y - k);
      p.lineTo(x + k, y + k * 0.8);
      p.lineTo(x - k, y + k * 0.8);
      p.closePath();
      return;
    }
    default:
      p.moveTo(x + r, y);
      p.arc(x, y, r, 0, 2 * Math.PI);
      if (type === 'relationship') {
        p.moveTo(x + r * 0.45, y);
        p.arc(x, y, r * 0.45, 0, 2 * Math.PI);
      }
  }
}

/**
 * A memory's look, as plain.css's cascade gives it (`.hub` < `.never` <
 * `.inv`, all one specificity, so the later wins per property; the focused
 * memory's outline above them): [fill, stroke, width, dashed].
 */
function lookOf(rec, focused) {
  const never = !reads(rec.n);
  const inv = !!rec.n.invalidated;
  const fill = inv ? 'bg2' : never ? 'bg' : 'node';
  const stroke = focused ? 'ink' : inv ? 'ink3' : never ? 'node' : rec.hub ? 'ink' : 'bg';
  const width = focused ? 1.8 : never || rec.hub ? 1.6 : 1.5;
  return [fill, stroke, width, inv];
}

const nodeCache = new WeakMap();

function nodeGroups(recs, focused, version) {
  const had = !focused && nodeCache.get(recs);
  if (had && had.version === version) return had;
  const looks = new Map();
  const dots = new Path2D();
  const chal = new Path2D();
  for (const rec of recs) {
    const lk = lookOf(rec, !!(focused && focused.has(rec.i)));
    // A polygon's own path per look: Path2D.closePath costs in proportion
    // to the path so far (`placedSink`), so they are kept apart.
    const poly = rec.n.type === 'hazard' || rec.n.type === 'debug';
    const key = lk.join('|') + (poly ? '|' + rec.n.type : '');
    let gp = looks.get(key);
    if (!gp) looks.set(key, (gp = { lk, path: new Path2D() }));
    shape(gp.path, rec.n.type, rec.x, rec.y, rec.r);
    if (rec.n.type === 'context') {
      dots.moveTo(rec.x + rec.r * 0.36, rec.y);
      dots.arc(rec.x, rec.y, rec.r * 0.36, 0, 2 * Math.PI);
    }
    if (rec.n.challenged) {
      chal.moveTo(rec.x + rec.r + 3.5, rec.y);
      chal.arc(rec.x, rec.y, rec.r + 3.5, 0, 2 * Math.PI);
    }
  }
  const out = { version, looks: [...looks.values()], dots, chal };
  if (!focused) nodeCache.set(recs, out);
  return out;
}

/**
 * Memories `recs` ({ i, n, x, y, r, hub }), batched by look. `o`: {
 * focused: Set of the memories outlined as focused (or null), version: the
 * host's count of hub changes, so a cached merge is dropped when one
 * moves }. The challenge ring goes under the shapes, as in the SVG.
 */
function drawNodes(ctx, recs, T, o) {
  if (!recs.length) return;
  const colour = { bg: T.bg, bg2: T.bg2, node: T.node, ink: T.ink, ink3: T.ink3 };
  const { looks, dots, chal } = nodeGroups(recs, o.focused || null, o.version);
  ctx.globalAlpha = 1;
  ctx.setLineDash([]);
  ctx.strokeStyle = T.red;
  ctx.lineWidth = 2.4;
  ctx.stroke(chal);
  for (const { lk, path } of looks) {
    const [fill, stroke, width, dashed] = lk;
    ctx.fillStyle = colour[fill];
    ctx.fill(path);
    ctx.strokeStyle = colour[stroke];
    ctx.lineWidth = width;
    ctx.setLineDash(dashed ? [2, 3] : []);
    ctx.stroke(path);
  }
  ctx.setLineDash([]);
  ctx.fillStyle = T.bg;
  ctx.fill(dots);
}

/** The hooks, by name (the host's contract: graph/canvas/host.js docs). */
export const canvas = Object.freeze({ look, tokens, backdrop, drawBelow, drawAbove, edge, edgeRank, drawEdges, drawNodes });
