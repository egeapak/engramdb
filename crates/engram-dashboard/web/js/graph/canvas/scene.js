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
// **Drawing by rectangle.** Every drawing here is of a list of device
// rectangles of a frame (clipped to their union), so the host redraws only
// what changed or what is on screen now, and the rest in idle time. Two part bitmaps hold what a
// display filter does not change (`PARTS`):
//   below   the style's backdrop under the relations (opaque: the background too)
//   over    the style's backdrop over the relations, the labels the zoom
//           shows, every memory
// The unfaded picture is below, the relations drawn straight in and over
// (`composeUnfaded`), or the same straight from the scene
// (`drawUnfaded`); the faded one (`drawFaded`) lays each group on at its
// own opacity through a scratch canvas the rectangles' size, so nothing
// faded is kept between drawings.
// An opened scope's stubs and tags are drawn straight in (a filter changes
// them, there are few, and a stub's faded opacity is not its full one times
// a constant).

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

const drawBelow = (ctx, S, fr) => S.hooks.drawBelow(ctx, backdropOf(S), S.T, hookFrame(S, fr));
const drawEdgesOf = (ctx, S, fr) => S.hooks.drawEdges(ctx, S.recs, S.T, { scale: fr.scale, lit: false });
const drawAbove = (ctx, S, fr) => S.hooks.drawAbove(ctx, backdropOf(S), S.T, hookFrame(S, fr));
const drawNodesOf = (ctx, S) => S.hooks.drawNodes(ctx, S.nodes, S.T, { focused: null, version: S.nodeVersion });

/** The parts (module docs): `opaque` (drawn over the background) and the drawing, in layout units. */
export const PARTS = Object.freeze({
  below: { opaque: true, draw: drawBelow },
  // (Drawn by `drawFaint`: its groups need a scratch canvas.)
  faint: { opaque: false, draw: null },
  over: {
    opaque: false,
    draw: (ctx, S, fr) => {
      drawAbove(ctx, S, fr);
      ctx.globalAlpha = 1;
      drawLabels(ctx, S, baseLabels(S, fr));
      ctx.globalAlpha = 1;
      drawNodesOf(ctx, S, fr);
    },
  },
});

/** The bounds of device rectangles `rs`. */
export function boundsOf(rs) {
  const b = [Infinity, Infinity, -Infinity, -Infinity];
  for (const r of rs) {
    if (r[0] < b[0]) b[0] = r[0];
    if (r[1] < b[1]) b[1] = r[1];
    if (r[2] > b[2]) b[2] = r[2];
    if (r[3] > b[3]) b[3] = r[3];
  }
  return b;
}

/** Clip `ctx` to the union of device rectangles `rs` of the frame, drawn at offset (dx, dy), with the identity transform set. */
function clipTo(ctx, rs, dx, dy) {
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = 1;
  ctx.beginPath();
  for (const r of rs) ctx.rect(r[0] - dx, r[1] - dy, r[2] - r[0], r[3] - r[1]);
  ctx.clip();
}
const fillRects = (ctx, rs, dx, dy) => {
  for (const r of rs) ctx.fillRect(r[0] - dx, r[1] - dy, r[2] - r[0], r[3] - r[1]);
};

/**
 * How far a memory's drawing reaches from its centre, in layout units: its
 * shape (a hazard's diamond: 1.25 radii) and stroke, or its challenge ring
 * (radius + 3.5, stroked 2.4 wide), with a little to spare.
 */
export const nodeReach = (nd) => Math.max(1.25 * nd.r + 1, nd.r + 4.7) + 0.5;

/** Device rectangle `r` of frame `fr` in layout units. */
function toLayout(fr, r) {
  const k = fr.dpr * fr.scale;
  const ox = (fr.M - fr.view.x * fr.scale) * fr.dpr;
  const oy = (fr.M - fr.view.y * fr.scale) * fr.dpr;
  return [(r[0] - ox) / k, (r[1] - oy) / k, (r[2] - ox) / k, (r[3] - oy) / k];
}
const meets = (b, qs) => qs.some((q) => b[0] < q[2] && b[2] > q[0] && b[1] < q[3] && b[3] > q[1]);

/**
 * `over` inside small rectangles (memories whose look changed): the
 * backdrop over the relations whole (under the clip), but only the labels
 * and memories that reach a rectangle, so a filter that changes a few
 * hundred memories' looks writes those, not every memory and label again.
 */
function drawOverIn(ctx, S, fr, rs) {
  const qs = rs.map((r) => toLayout(fr, r));
  drawAbove(ctx, S, fr);
  ctx.globalAlpha = 1;
  drawLabels(ctx, S, baseLabels(S, fr).filter((it) => meets(labelBox(S, it.i, it.x, it.y, it.anchor, 600), qs)));
  ctx.globalAlpha = 1;
  const recs = S.nodes.filter((nd) => {
    const q = nodeReach(nd);
    return meets([nd.x - q, nd.y - q, nd.x + q, nd.y + q], qs);
  });
  if (recs.length) S.hooks.drawNodes(ctx, recs, S.T, { focused: null, version: -1 });
}

/**
 * Draw part `name` of frame `fr` inside device rectangles `rs` (cleared
 * there first); `few`: they are small (memories whose look changed), so
 * `over` draws only what reaches them.
 */
export function drawPart(ctx, S, fr, name, rs, few, scratch) {
  const p = PARTS[name];
  ctx.save();
  clipTo(ctx, rs, 0, 0);
  if (p.opaque) {
    ctx.fillStyle = S.T.bg;
    fillRects(ctx, rs, 0, 0);
  } else for (const r of rs) ctx.clearRect(r[0], r[1], r[2] - r[0], r[3] - r[1]);
  viewTransform(ctx, fr);
  if (name === 'faint') drawFaint(ctx, S, fr, rs, few, scratch);
  else if (few && name === 'over') drawOverIn(ctx, S, fr, rs);
  else p.draw(ctx, S, fr);
  ctx.restore();
}

/**
 * Group `draw` (in layout units) drawn whole into `scratch` inside device
 * rectangles `rs` of `fr`, then laid onto `ctx` at `alpha` and offset
 * (dx, dy): a group's own overlaps do not show through, as an SVG group's
 * opacity.
 */
function group(ctx, scratch, S, fr, rs, draw, alpha, dx = 0, dy = 0) {
  const r = boundsOf(rs);
  const w = r[2] - r[0];
  const hh = r[3] - r[1];
  const sx = scratch.getContext('2d');
  sx.save();
  clipTo(sx, rs, r[0], r[1]);
  sx.clearRect(0, 0, w, hh);
  viewTransform(sx, fr, r[0], r[1]);
  draw(sx);
  sx.restore();
  ctx.save();
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = alpha;
  ctx.drawImage(scratch, 0, 0, w, hh, r[0] - dx, r[1] - dy, w, hh);
  ctx.restore();
}

/** Labels faded (look.fadeLabel), then memories faded (look.fadeNode), each as a group: the `faint` part. */
function drawFaint(ctx, S, fr, rs, few, scratch) {
  const lk = S.look;
  const qs = few ? rs.map((r) => toLayout(fr, r)) : null;
  let lbl = baseLabels(S, fr);
  if (qs) lbl = lbl.filter((it) => meets(labelBox(S, it.i, it.x, it.y, it.anchor, 600), qs));
  if (lbl.length) group(ctx, scratch, S, fr, rs, (c) => drawLabels(c, S, lbl), lk.fadeLabel);
  const recs = qs
    ? S.nodes.filter((nd) => {
        const q = nodeReach(nd);
        return meets([nd.x - q, nd.y - q, nd.x + q, nd.y + q], qs);
      })
    : null;
  if (!recs) group(ctx, scratch, S, fr, rs, (c) => drawNodesOf(c, S, fr), lk.fadeNode);
  else if (recs.length) group(ctx, scratch, S, fr, rs, (c) => S.hooks.drawNodes(c, recs, S.T, { focused: null, version: -1 }), lk.fadeNode);
}

/**
 * The unfaded picture inside device rectangles `rs` of frame `fr`, straight
 * from the scene (no part involved): for the strips a pan uncovers and a
 * frame no part holds yet. What `composeUnfaded` gives, primitive by
 * primitive (source-over is associative, so it differs from the blits by
 * rounding only).
 */
export function drawUnfaded(ctx, S, fr, rs) {
  ctx.save();
  clipTo(ctx, rs, 0, 0);
  ctx.fillStyle = S.T.bg;
  fillRects(ctx, rs, 0, 0);
  viewTransform(ctx, fr);
  PARTS.below.draw(ctx, S, fr);
  ctx.globalAlpha = 1;
  if (S.stubs.length) drawStubs(ctx, S, fr, 'full');
  ctx.globalAlpha = 1;
  drawEdgesOf(ctx, S, fr);
  ctx.globalAlpha = 1;
  PARTS.over.draw(ctx, S, fr);
  ctx.globalAlpha = 1;
  if (S.tags.length) drawTags(ctx, S, fr, -1, -1, false);
  ctx.restore();
}

/**
 * The unfaded picture inside device rectangles `rs` from the parts `P`
 * ({ below, over }: canvases of frame `fr`, current there): below, the
 * stubs and relations drawn straight in, over, the tags. A display filter
 * costs this: the relations and two blits.
 */
export function composeUnfaded(ctx, S, fr, rs, P) {
  const r = boundsOf(rs);
  const w = r[2] - r[0];
  const hh = r[3] - r[1];
  ctx.save();
  clipTo(ctx, rs, 0, 0);
  ctx.drawImage(P.below, r[0], r[1], w, hh, r[0], r[1], w, hh);
  viewTransform(ctx, fr);
  if (S.stubs.length) drawStubs(ctx, S, fr, 'full');
  ctx.globalAlpha = 1;
  drawEdgesOf(ctx, S, fr);
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = 1;
  ctx.drawImage(P.over, r[0], r[1], w, hh, r[0], r[1], w, hh);
  if (S.tags.length) {
    viewTransform(ctx, fr);
    drawTags(ctx, S, fr, -1, -1, false);
  }
  ctx.restore();
}

/**
 * What a focus shows outside the patch, inside device rectangles `rs` of
 * frame `fr`, drawn into `ctx` at offset (dx, dy): the backdrop, then the
 * relations, labels and memories each faded as a group (the SVG fades a
 * group: each is drawn whole into `o.scratch`, a canvas at least the size
 * of the rectangles' bounds, and laid on at the style's faded opacity); the
 * tags never fade. `o`: { scratch, below (the `below` part of `fr`, current
 * there, or null: drawn from the scene), skip (memories whose faded label
 * is left out: the lit ones, under the patch), dx, dy }.
 */
export function drawFaded(ctx, S, fr, rs, o) {
  const lk = S.look;
  const fb = lk.fadeBackdrop !== undefined ? lk.fadeBackdrop : 1;
  const dx = o.dx || 0;
  const dy = o.dy || 0;
  const r = boundsOf(rs);
  const w = r[2] - r[0];
  const hh = r[3] - r[1];
  const sc = o.scratch;
  const sx = sc.getContext('2d');
  // Group `draw` (in layout units) laid on at `alpha`.
  const group = (draw, alpha) => {
    sx.save();
    clipTo(sx, rs, r[0], r[1]);
    sx.clearRect(0, 0, w, hh);
    viewTransform(sx, fr, r[0], r[1]);
    draw(sx);
    sx.restore();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = alpha;
    ctx.drawImage(sc, 0, 0, w, hh, r[0] - dx, r[1] - dy, w, hh);
    ctx.globalAlpha = 1;
  };
  ctx.save();
  clipTo(ctx, rs, dx, dy);
  ctx.fillStyle = S.T.bg;
  fillRects(ctx, rs, dx, dy);
  if (o.below) {
    ctx.globalAlpha = fb;
    ctx.drawImage(o.below, r[0], r[1], w, hh, r[0] - dx, r[1] - dy, w, hh);
    ctx.globalAlpha = 1;
  } else if (fb === 1) {
    viewTransform(ctx, fr, dx, dy);
    drawBelow(ctx, S, fr);
    ctx.globalAlpha = 1;
  } else group((c) => drawBelow(c, S, fr), fb);
  if (S.stubs.length) {
    viewTransform(ctx, fr, dx, dy);
    drawStubs(ctx, S, fr, 'faded');
  }
  group((c) => drawEdgesOf(c, S, fr), lk.fadeEdge);
  if (fb === 1) {
    viewTransform(ctx, fr, dx, dy);
    drawAbove(ctx, S, fr);
    ctx.globalAlpha = 1;
  } else group((c) => drawAbove(c, S, fr), fb);
  if (o.faint) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.drawImage(o.faint, r[0], r[1], w, hh, r[0] - dx, r[1] - dy, w, hh);
  } else {
    const lbl = baseLabels(S, fr);
    const kept = o.skip ? lbl.filter((it) => !o.skip.has(it.i)) : lbl;
    if (kept.length) group((c) => drawLabels(c, S, kept), lk.fadeLabel);
    group((c) => drawNodesOf(c, S, fr), lk.fadeNode);
  }
  if (S.tags.length) {
    viewTransform(ctx, fr, dx, dy);
    drawTags(ctx, S, fr, -1, -1, false);
  }
  ctx.restore();
}
