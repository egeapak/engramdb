// What a canvas layer of the graph shows, drawn from plain data: the part
// of the canvas surface that runs wherever the drawing happens, in the
// raster worker (./raster-worker.js) or, without one, on the page
// (./raster.js). No DOM here: a scene is the graph's memories, relations,
// stubs and tags, the style's hooks and the tokens, and every layer is a
// function of a scene and a frame.
//
// A frame `fr` is { view: { x, y, w, h }, scale (screen px per layout
// unit), M (the overscan margin, CSS px), dpr, pw, ph (device size), px
// (device px per layout unit), font }.
//
// Layers (host.js says which is shown when):
//   base      everything, unfaded
//   dimBack   the backdrop, the faded relations and stubs, the scope names
//   dimNodes  every memory, unfaded, on transparent (blitted at the style's
//             faded opacity: one opacity per layer, as the SVG fades a group)
//   dim       dimBack + every label and memory at their faded opacities,
//             and the tags: what a focus shows outside the patch

import { fmt } from '../../format.js';
import { scopeName } from '../../scopes.js';
import { radius } from '../radius.js';

/** Stub opacities, from app.css `.xs-stub` (full / many, under a focus, lit). */
export const STUB = { full: [0.3, 0.4], many: [0.09, 0.16], faded: [0.08, 0.08], lit: [0.8, 0.8] };

/** Label size and halo (app.css `.lbl`). */
const LABEL_PX = 11.5;

/**
 * A scene for graph `g` (only `g.nodes` is read) laid out as `lay` (its
 * `routes` are not needed: relations come with their points), `labels[i]`
 * memory i's label text, `hooks` the style's canvas hooks.
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
  };
}

/** The style's backdrop, prepared on first use. */
export function backdropOf(S) {
  return S.bd || (S.bd = S.hooks.backdrop(S.g, S.lay));
}

export function setTokens(S, T, font) {
  S.T = T;
  S.font = font;
  if (S.bd) S.bd.patterns.clear();
}

/** Hub flags (`hubs[i]` 1 or 0) after the view counted adjacency. */
export function setHubs(S, hubs, version) {
  for (let i = 0; i < S.nodes.length; i++) S.nodes[i].hub = !!hubs[i];
  S.nodeVersion = version;
}

/** Relations shown, in index order: [{ e, pts, ends }] → the style's records. */
export function setEdges(S, items) {
  const recs = [];
  for (const it of items) {
    const rec = S.hooks.edge(it.e, it.pts, it.ends);
    if (rec) recs.push(rec);
  }
  S.recs = recs;
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

export function viewTransform(ctx, fr) {
  const px = fr.dpr * fr.scale;
  ctx.setTransform(px, 0, 0, px, (fr.M - fr.view.x * fr.scale) * fr.dpr, (fr.M - fr.view.y * fr.scale) * fr.dpr);
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

/** A label's box in layout units (its text estimated, halo included). */
export function labelBox(S, i, x, y, anchor) {
  const w = S.labels[i].length * LABEL_PX * 0.68 + 8;
  const x0 = anchor === 'end' ? x - w : anchor === 'middle' ? x - w / 2 : x - 4;
  return [x0, y - 14, x0 + w, y + 6];
}

export function drawLabels(ctx, S, items) {
  if (!items.length) return;
  ctx.globalAlpha = 1;
  ctx.lineJoin = 'round';
  ctx.lineWidth = 4;
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

export function drawBase(ctx, S, fr) {
  begin(ctx, S, fr, true);
  const bd = backdropOf(S);
  const f = hookFrame(S, fr);
  S.hooks.drawBelow(ctx, bd, S.T, f);
  drawStubs(ctx, S, fr, 'full');
  S.hooks.drawEdges(ctx, S.recs, S.T, { scale: fr.scale, zk: 1, alpha: 1 });
  S.hooks.drawAbove(ctx, bd, S.T, f);
  drawLabels(ctx, S, baseLabels(S, fr));
  S.hooks.drawNodes(ctx, S.nodes, S.T, { focused: null, version: S.nodeVersion });
  drawTags(ctx, S, fr, -1, -1, false);
}

export function drawDimBack(ctx, S, fr) {
  begin(ctx, S, fr, true);
  const bd = backdropOf(S);
  const f = hookFrame(S, fr);
  S.hooks.drawBelow(ctx, bd, S.T, f);
  drawStubs(ctx, S, fr, 'faded');
  S.hooks.drawEdges(ctx, S.recs, S.T, { scale: fr.scale, zk: 1, alpha: S.look.fadeEdge });
  S.hooks.drawAbove(ctx, bd, S.T, f);
}

export function drawDimNodes(ctx, S, fr) {
  begin(ctx, S, fr, false);
  S.hooks.drawNodes(ctx, S.nodes, S.T, { focused: null, version: S.nodeVersion });
}

/** Every unlit label on transparent (`skip`: a Set of memories left out). */
export function drawDimLabels(ctx, S, fr, skip) {
  begin(ctx, S, fr, false);
  drawLabels(ctx, S, skip ? baseLabels(S, fr).filter((it) => !skip.has(it.i)) : baseLabels(S, fr));
}

/**
 * `dim` from its parts: dimBack, then the labels and the memories at their
 * faded opacities, then the far-scope tags unfaded (a focus never fades them).
 */
export function composeDim(ctx, S, fr, back, labels, nodes) {
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = 1;
  ctx.drawImage(back, 0, 0);
  ctx.globalAlpha = S.look.fadeLabel;
  ctx.drawImage(labels, 0, 0);
  ctx.globalAlpha = S.look.fadeNode;
  ctx.drawImage(nodes, 0, 0);
  ctx.globalAlpha = 1;
  viewTransform(ctx, fr);
  drawTags(ctx, S, fr, -1, -1, false);
}
