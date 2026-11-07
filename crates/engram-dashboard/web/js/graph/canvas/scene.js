// What a canvas layer of the graph shows, drawn from plain data: the
// graph's memories, relations, stubs and tags, the style's hooks and the
// tokens. The host (./host.js) owns the canvases; everything here draws
// into a context it is handed, and the only page state it reads is none
// (the one canvas it makes, for measuring label text, is never in the
// page; a style's hooks may make their own, as Plain's textures do).
//
// A frame `fr` is { view: { x, y, w, h }, scale (screen px per layout
// unit), M (the overscan margin, CSS px), dpr, pw, ph (device size), font }.
//
// **Parts.** Each layer the page shows is composed from part bitmaps, each
// drawn by one function of the scene and a frame (`PARTS`), so a change
// redraws only the parts it touches (a display filter: the relations) and
// the layers are blits (`compose`):
//   below   the style's backdrop under the relations (opaque: the background too)
//   edges   every shown relation, unfaded (faded as a group, by its blit's opacity)
//   above   the style's backdrop over the relations
//   labels  the labels the zoom shows
//   nodes   every memory, unfaded
//   over    above, labels and nodes in one (the unfaded layer's top)
// The unfaded layer is below, its relations drawn straight in, and over;
// the faded copy blits below, edges, above, labels and nodes, each at its
// own opacity (`LAYER_PARTS`).
// The opened scope's stubs and tags are drawn straight into each layer
// (few, and a stub's faded opacity is not its full one times a constant).

import { fmt } from '../../format.js';
import { scopeName } from '../../scopes.js';
import { radius } from '../radius.js';

/** Stub opacities, from app.css `.xs-stub` (full / many, under a focus, lit). */
export const STUB = { full: [0.3, 0.4], many: [0.09, 0.16], faded: [0.08, 0.08], lit: [0.8, 0.8] };

/** Label size and halo (app.css `.lbl`). */
const LABEL_PX = 11.5;
const HALO = 4;

/**
 * A scene for graph `g` (only `g.nodes` is read) laid out as `lay`,
 * `labels[i]` memory i's label text, `hooks` the style's canvas hooks.
 */
export function makeScene(hooks, g, lay, labels) {
  const nodes = g.nodes.map((n, i) => ({ i, n, x: lay.pos[2 * i], y: lay.pos[2 * i + 1], r: radius(n), hub: n.degree >= 2 }));
  return {
    hooks,
    look: hooks.look,
    g,
    lay,
    labels,
    nodes,
    major: g.nodes.map((n) => n.degree >= 2 || n.proposed > 0 || n.challenged),
    bd: null,
    recs: [],
    stubs: [],
    stubsMany: false,
    tags: [],
    T: null,
    font: 'sans-serif',
    nodeVersion: 0,
    widths: new Map(),
    measure: null,
  };
}

/** The style's backdrop, prepared on first use. */
export function backdropOf(S) {
  return S.bd || (S.bd = S.hooks.backdrop(S.g, S.lay));
}

/** New tokens and font (a light/dark or style switch). A style keys its own caches by the tokens. */
export function setTokens(S, T, font) {
  S.T = T;
  if (font !== S.font) S.widths.clear();
  S.font = font;
}

/** Clear `ctx` (to the background when `opaque`) and set the frame's view transform. */
export function begin(ctx, S, fr, opaque) {
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = 1;
  if (opaque) {
    ctx.fillStyle = S.T.bg;
    ctx.fillRect(0, 0, fr.pw, fr.ph);
  } else ctx.clearRect(0, 0, fr.pw, fr.ph);
  viewTransform(ctx, fr);
  return ctx;
}

/** The frame's view transform, shifted by (dx, dy) device px (a patch's corner). */
export function viewTransform(ctx, fr, dx = 0, dy = 0) {
  const px = fr.dpr * fr.scale;
  ctx.setTransform(px, 0, 0, px, (fr.M - fr.view.x * fr.scale) * fr.dpr - dx, (fr.M - fr.view.y * fr.scale) * fr.dpr - dy);
}

/** The drawn area in layout units (the box and its margin), widened by `pad`. */
function area(fr, pad) {
  const m = fr.M / fr.scale + pad;
  return [fr.view.x - m, fr.view.y - m, fr.view.x + fr.view.w + m, fr.view.y + fr.view.h + m];
}

/** The labels the view shows unlit (placed, and the zoom's rule: .z-near all, .z-mid the major ones). */
export function baseLabels(S, fr) {
  if (fr.scale < 0.5) return [];
  const [x0, y0, x1, y1] = area(fr, 300);
  const out = [];
  for (let i = 0; i < S.nodes.length; i++) {
    const lp = S.lay.labels[i];
    if (!lp || !lp.placed || (fr.scale < 0.8 && !S.major[i])) continue;
    if (lp.x < x0 || lp.x > x1 || lp.y < y0 || lp.y > y1) continue;
    out.push({ i, x: lp.x, y: lp.y, anchor: lp.anchor, weight: 600 });
  }
  return out;
}

/** Label i's text width at `weight`, measured once (the font is the box's). */
function labelWidth(S, i, weight) {
  const key = i * 4 + (weight >= 700 ? 2 : weight >= 600 ? 1 : 0);
  let w = S.widths.get(key);
  if (w !== undefined) return w;
  if (!S.measure) {
    const c = typeof OffscreenCanvas === 'function' ? new OffscreenCanvas(1, 1) : document.createElement('canvas');
    S.measure = { ctx: c.getContext('2d'), weight: 0 };
  }
  const m = S.measure;
  if (m.weight !== weight || m.font !== S.font) {
    m.ctx.font = `${weight} ${LABEL_PX}px ${S.font}`;
    m.weight = weight;
    m.font = S.font;
  }
  w = m.ctx.measureText(S.labels[i]).width;
  S.widths.set(key, w);
  return w;
}

/** A label's box in layout units, its text measured at `weight` (the boldest lit one by default), halo included. */
export function labelBox(S, i, x, y, anchor, weight = 700) {
  const w = labelWidth(S, i, weight) + HALO + 2;
  const x0 = anchor === 'end' ? x - w + 1 : anchor === 'middle' ? x - w / 2 : x - HALO / 2 - 1;
  return [x0, y - 14, x0 + w, y + 6];
}

export function drawLabels(ctx, S, items) {
  if (!items.length) return;
  ctx.globalAlpha = 1;
  ctx.lineJoin = 'round';
  ctx.lineWidth = HALO;
  ctx.strokeStyle = S.T.bg;
  ctx.fillStyle = S.T.ink;
  ctx.textBaseline = 'alphabetic';
  let weight = 0;
  for (const it of items) {
    if (it.weight !== weight) {
      weight = it.weight;
      ctx.font = `${weight} ${LABEL_PX}px ${S.font}`;
    }
    ctx.textAlign = it.anchor === 'end' ? 'right' : it.anchor === 'middle' ? 'center' : 'left';
    ctx.strokeText(S.labels[it.i], it.x, it.y);
    ctx.fillText(S.labels[it.i], it.x, it.y);
  }
}

/** The stub lines (app.css `.xs-stub`): `state` full | faded | lit, `only` a memory's own. */
export function drawStubs(ctx, S, fr, state, only) {
  if (!S.stubs.length) return;
  const op = state === 'lit' ? STUB.lit : S.stubsMany ? STUB.many : state === 'faded' ? STUB.faded : STUB.full;
  for (const contra of [false, true]) {
    ctx.strokeStyle = contra ? S.T.red : S.T.ink3;
    ctx.globalAlpha = op[contra ? 1 : 0];
    for (const st of S.stubs) {
      if (!st.contra !== !contra || (only !== undefined && st.i !== only)) continue;
      ctx.lineWidth = st.w / fr.scale;
      ctx.beginPath();
      ctx.moveTo(st.x1, st.y1);
      ctx.lineTo(st.x2, st.y2);
      ctx.stroke();
    }
  }
  ctx.globalAlpha = 1;
}

/**
 * The far-scope tags (app.css `.xs-tag`): `hot` the hovered one (lighter),
 * `focused` the focused one (ringed); `onlyMarked` draws just those two.
 */
export function drawTags(ctx, S, fr, hot, focused, onlyMarked) {
  if (!S.tags.length) return;
  const T = S.T;
  ctx.textBaseline = 'alphabetic';
  S.tags.forEach((tag, j) => {
    const isHot = j === hot;
    const isFocused = j === focused;
    if (onlyMarked && !isHot && !isFocused) return;
    const { tg } = tag;
    ctx.globalAlpha = 1;
    ctx.beginPath();
    ctx.roundRect(tg.x - tg.w / 2, tg.y - tg.h / 2, tg.w, tg.h, tg.h / 2);
    ctx.fillStyle = isHot ? T.bg2 : T.bg;
    ctx.fill();
    ctx.strokeStyle = isFocused ? T.focus : isHot ? T.ink : tag.contradictions ? T.red : T.ink3;
    ctx.lineWidth = (isFocused ? 3 : 1) / fr.scale;
    ctx.stroke();
    ctx.font = `600 ${tg.font}px ${S.font}`;
    ctx.textAlign = 'left';
    const a = `${scopeName(tag.name)} · ${fmt(tag.count)}`;
    // One space before the sign: the SVG collapses the tspan's two.
    const b = tag.contradictions ? ` ⚠ ${fmt(tag.contradictions)}` : '';
    const wa = ctx.measureText(a).width;
    const wb = b ? ctx.measureText(b).width : 0;
    const x = tg.x - (wa + wb) / 2;
    const y = tg.y + tg.font * 0.36;
    ctx.fillStyle = T.ink2;
    ctx.fillText(a, x, y);
    if (b) {
      ctx.fillStyle = T.redInk;
      ctx.fillText(b, x + wa, y);
    }
  });
}

const hookFrame = (S, fr) => ({ scale: fr.scale, px: fr.dpr * fr.scale, font: S.font });

const drawAbovePart = (ctx, S, fr) => S.hooks.drawAbove(ctx, backdropOf(S), S.T, hookFrame(S, fr));
const drawLabelsPart = (ctx, S, fr) => drawLabels(ctx, S, baseLabels(S, fr));
const drawNodesPart = (ctx, S) => S.hooks.drawNodes(ctx, S.nodes, S.T, { focused: null, version: S.nodeVersion });

/** The parts (module docs): `opaque` (drawn over the background) and the drawing, in layout units. */
export const PARTS = Object.freeze({
  below: { opaque: true, draw: (ctx, S, fr) => S.hooks.drawBelow(ctx, backdropOf(S), S.T, hookFrame(S, fr)) },
  edges: { opaque: false, draw: (ctx, S, fr) => S.hooks.drawEdges(ctx, S.recs, S.T, { scale: fr.scale, lit: false }) },
  above: { opaque: false, draw: drawAbovePart },
  labels: { opaque: false, draw: drawLabelsPart },
  nodes: { opaque: false, draw: drawNodesPart },
  over: {
    opaque: false,
    draw: (ctx, S, fr) => {
      drawAbovePart(ctx, S, fr);
      ctx.globalAlpha = 1;
      drawLabelsPart(ctx, S, fr);
      ctx.globalAlpha = 1;
      drawNodesPart(ctx, S, fr);
    },
  },
});

/**
 * The parts each layer is composed from. The unfaded layer draws its
 * relations straight in, between two blits (a display filter then costs
 * those relations and two blits); the faded copy fades each group on its
 * own, so it blits each part at its own opacity.
 */
export const LAYER_PARTS = Object.freeze({ base: Object.freeze(['below', 'over']), dim: Object.freeze(['below', 'edges', 'above', 'labels', 'nodes']) });

/**
 * The unfaded layer drawn straight from the scene, no part involved: what
 * `compose(.., false)` gives, primitive by primitive (source-over is
 * associative, so it differs from the blits by rounding only). For the
 * strips a pan uncovers and a settle at a new scale, where no part holds
 * the frame yet.
 */
export function drawUnfaded(ctx, S, fr) {
  begin(ctx, S, fr, true);
  PARTS.below.draw(ctx, S, fr);
  ctx.globalAlpha = 1;
  if (S.stubs.length) drawStubs(ctx, S, fr, 'full');
  ctx.globalAlpha = 1;
  PARTS.edges.draw(ctx, S, fr);
  ctx.globalAlpha = 1;
  PARTS.over.draw(ctx, S, fr);
  ctx.globalAlpha = 1;
  if (S.tags.length) drawTags(ctx, S, fr, -1, -1, false);
}

/** Draw part `name` of frame `fr` into `ctx` (cleared first: within the clip, when one is set). */
export function drawPart(ctx, S, fr, name) {
  const p = PARTS[name];
  begin(ctx, S, fr, p.opaque);
  p.draw(ctx, S, fr);
  ctx.globalAlpha = 1;
}

/**
 * A layer from the parts `P` (canvases of frame `fr`, `LAYER_PARTS`):
 * `base` unfaded (below, the relations drawn straight in, `over`), or
 * `faded` (what a focus shows outside the patch: each part at the style's
 * faded opacity, as the SVG fades a group; the tags never fade). Drawn at
 * device offset (dx, dy) (a patch's corner), with `labels` in place of
 * P.labels when given, at that offset already (faded only).
 */
export function compose(ctx, S, fr, P, faded, dx = 0, dy = 0, labels = null) {
  const px = -dx;
  const py = -dy;
  const lk = S.look;
  const fb = faded && lk.fadeBackdrop !== undefined ? lk.fadeBackdrop : 1;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = 1;
  if (fb < 1) {
    ctx.fillStyle = S.T.bg;
    ctx.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  }
  ctx.globalAlpha = fb;
  ctx.drawImage(P.below, px, py);
  if (S.stubs.length) {
    viewTransform(ctx, fr, dx, dy);
    drawStubs(ctx, S, fr, faded ? 'faded' : 'full');
    ctx.setTransform(1, 0, 0, 1, 0, 0);
  }
  if (!faded) {
    viewTransform(ctx, fr, dx, dy);
    PARTS.edges.draw(ctx, S, fr);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.drawImage(P.over, px, py);
  } else {
    ctx.globalAlpha = lk.fadeEdge;
    ctx.drawImage(P.edges, px, py);
    ctx.globalAlpha = fb;
    ctx.drawImage(P.above, px, py);
    ctx.globalAlpha = lk.fadeLabel;
    if (labels) ctx.drawImage(labels, 0, 0);
    else ctx.drawImage(P.labels, px, py);
    ctx.globalAlpha = lk.fadeNode;
    ctx.drawImage(P.nodes, px, py);
  }
  ctx.globalAlpha = 1;
  if (S.tags.length) {
    viewTransform(ctx, fr, dx, dy);
    drawTags(ctx, S, fr, -1, -1, false);
  }
}
