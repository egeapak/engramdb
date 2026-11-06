// The canvas graph surface: the core host a canvas style draws through (a
// style with `renderer = 'canvas'` and a `canvas` hooks object; Plain's are
// themes/plain/canvas.js). Same members as the SVG surface
// (../surface-svg.js docs); views/graph.js cannot tell them apart except
// by `composited`.
//
// **What the host owns** (so a second canvas style supplies draw hooks
// only): the canvases and their sizing, the layering below, the view
// transform, hit testing (./hit.js), the accessible list, the focus and
// hover rings, the stub lines and far-scope tags of an opened scope, the
// memory labels, token reading and the light/dark redraw. **What a style
// supplies** (`theme.canvas`):
//
//   look         { fadeNode, fadeEdge, fadeLabel, litEdge, hitWidth,
//                  labelWeight: { focus, nb } }: its CSS's highlight
//                  numbers (faded opacities, a lit relation's extra width,
//                  the relation click target in screen px, lit labels'
//                  weights)
//   tokens(read) its colours, from custom properties (`read(name)`)
//   backdrop(g, lay)                  prepared once per graph
//   drawBelow(ctx, bd, T, fr), drawAbove(ctx, bd, T, fr)  under / over the relations
//   edge(e, pts, ends)                one relation's record (null: none)
//   drawEdges(ctx, recs, T, { scale, zk, alpha })
//   drawNodes(ctx, recs, T, { focused, version })  recs: { i, n, x, y, r, hub }
//
// `fr` (the frame) is { scale, px: device px per layout unit, font }; `T`
// is the core tokens (bg, bg2, ink, ink2, ink3, red, redInk, focus) plus
// the style's. Every hook draws in layout units: the host has set the
// view transform. The hooks draw only: the layers are a function of the
// scene (./scene.js), which holds no DOM.
//
// **Layers** (./scene.js draws them; each the view's box plus an overscan
// margin, at devicePixelRatio, capped). In the page, at most two at once:
//   base      everything, unfaded: the graph with nothing focused
//   dim       opaque, in place of `base` while something is focused: the
//             backdrop, faded relations, stubs and scope names, and every
//             label and memory at its faded opacity
//   patch     a small canvas over either, just the rectangle the focus,
//             rings and marked tags cover: what is under it redrawn (with a
//             focus, `dim` minus the lit memories' faded labels), then the
//             lit memories, relations and labels, the tags, the rings
// Each canvas in the page is a compositor layer whose whole backing store
// is re-sent on every frame that changes it (software compositing, the
// headless and many real cases: measured in
// docs/contributors/dashboard-performance.md), so a hover changes only the
// patch: at 2× it re-sent a full-size overlay and took twice the SVG's
// hover frame. `dim` and the two scratch canvases the patch copies from
// (`dimBack`: dim's backdrop and relations; `dimNodes`: every memory,
// unfaded) are drawn in an idle callback once `base` is up, or at the first
// focus if that comes first.
//
// (A raster worker drawing these layers off the page's thread was built
// and measured: it lost the first paint, 96 against 53 ms at 1k memories,
// docs/contributors/dashboard-performance.md.)
//
// **Zoom and pan.** The view's composited gesture moves the canvases (CSS
// transform on `.g-pane`) and nothing is drawn until it settles (`gesture`
// first draws the patch into the layer under it, so one canvas moves); the
// overscan margin is what a pan or zoom-out shows before then. A settle
// draws synchronously in the task that removes the transform, so no frame
// shows the old drawing unmoved or a blank one. The view runs the relation
// rebuild rules (cut mode, `redrawOnZoom`) in that same task.
//
// **Accessibility.** The canvas is `role="img"` with a label; every drawn
// memory is a `<button>` (the SVG `<g>`'s aria label, `tabindex=-1`,
// roving focus by ] / [) and every far-scope tag a `role="link"`, in an
// invisible list (opacity 0, no pointer events) placed over the drawing
// by one transform per view, so a screen magnifier following focus lands
// on the memory. Relations stay reachable through the relations table, as
// with the SVG surface (whose relations are aria-hidden too).

import { h, afterPaint } from '../../dom.js';
import { nodeAria } from '../a11y.js';
import { nodeGrid, edgeGrid } from './hit.js';
import * as sc from './scene.js';

/** devicePixelRatio is used up to this. */
const DPR_MAX = 2;
/** At most this many device pixels per canvas: a larger box draws at a lower ratio. */
const MAX_PIXELS = 6e6;
/** The overscan margin, as a share of the box's larger side (each side). */
const OVERSCAN = 0.12;
/** How far a memory catches the pointer beyond its radius (its ring's outer edge, as in the SVG). */
const RING_REACH = 7.5;
const CORE_TOKENS = { bg: '--bg', bg2: '--bg-2', ink: '--ink', ink2: '--ink-2', ink3: '--ink-3', red: '--red', redInk: '--red-ink', focus: '--focus' };

/** Whether this browser has what the host draws with. */
export function canvasSupported() {
  return typeof CanvasRenderingContext2D === 'function' && typeof Path2D === 'function' && typeof DOMMatrix === 'function' && 'addPath' in Path2D.prototype && 'roundRect' in CanvasRenderingContext2D.prototype;
}

/**
 * The surface, or null when a 2D context cannot be had (the view then
 * draws the style's SVG hooks). `o`: { g, lay, labels, theme, box (the
 * graph box), boxSize (kept current by the view), ariaLabel, cb } (`cb` as
 * for the SVG surface).
 */
export function canvasSurface(o) {
  const { g, lay, labels, theme, box, boxSize, cb } = o;
  const hooks = theme.canvas;
  const look = hooks.look;
  const layer = (cls, alpha) => {
    const cv = h('canvas', { class: cls, 'aria-hidden': 'true' });
    return { cv, ctx: null, alpha, w: 0, h: 0, key: null };
  };
  const L = { base: layer('g-cv-base', false), dim: layer('g-cv-dim', false), patch: layer('g-cv-patch', true) };
  L.base.cv.removeAttribute('aria-hidden');
  L.base.cv.setAttribute('role', 'img');
  L.base.cv.setAttribute('aria-label', o.ariaLabel);
  L.base.ctx = L.base.cv.getContext('2d', { alpha: false });
  if (!L.base.ctx) return null;
  for (const k of ['base', 'dim', 'patch']) L[k].cv.hidden = true;
  const ctxOf = (ly) => ly.ctx || (ly.ctx = ly.cv.getContext('2d', { alpha: ly.alpha }));

  // ---- data ------------------------------------------------------------------
  const S = sc.makeScene(hooks, g, lay, labels);
  const nodes = S.nodes;
  const hitNode = nodeGrid(lay.pos, nodes.map((nd) => nd.r + RING_REACH));
  let built = [];
  let want = new Uint8Array(0);
  let hitEdge = null;
  let tags = [];
  let view = null;
  let scale = 1;
  let plan = null;
  /** What the big layers show changed (relations, hubs, stubs, tags, tokens): their key moves. */
  let content = 0;

  // ---- tokens ------------------------------------------------------------------
  function readTokens() {
    const cs = getComputedStyle(document.body);
    const read = (name) => cs.getPropertyValue(name).trim();
    const core = {};
    for (const [k, name] of Object.entries(CORE_TOKENS)) core[k] = read(name);
    sc.setTokens(S, { ...core, ...hooks.tokens(read) }, getComputedStyle(box).fontFamily || 'sans-serif');
    content++;
  }
  readTokens();

  // ---- the DOM -------------------------------------------------------------------
  const list = h('div', { class: 'g-a11y', role: 'group', 'aria-label': 'Memories' });
  // One button per memory, built just after the first frame (styling 800
  // of them showed in the first paint, measured in
  // docs/contributors/dashboard-performance.md), or at once when keyboard
  // focus reaches the graph or the view focuses a memory, whichever comes
  // first: nothing can reach the list before it is complete. By hand
  // rather than through h(): the generic attribute walk showed too.
  let buttons = null;
  let listQueued = false;
  function ensureList() {
    if (buttons) return buttons;
    buttons = nodes.map((nd) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'g-a11y-node';
      b.tabIndex = -1;
      b.dataset.i = nd.i;
      b.setAttribute('aria-label', nodeAria(nd.n));
      const q = nd.r + 6;
      b.style.left = nd.x - q + 'px';
      b.style.top = nd.y - q + 'px';
      b.style.width = b.style.height = 2 * q + 'px';
      return b;
    });
    list.prepend(...buttons);
    return buttons;
  }
  box.addEventListener('focusin', ensureList);
  let tagEls = [];
  const host = h('div', { class: 'graph g-canvas' }, L.base.cv, L.dim.cv, L.patch.cv, list);

  // ---- frames ----------------------------------------------------------------------
  /** The frame for the box and view now: margin, ratio, device size (scene.js docs). */
  function frame() {
    const W = boxSize.w || 800;
    const H = boxSize.h || 500;
    // The first drawing is the box alone, the margin added once the page is
    // idle: it was nearly half the first paint's pixels (measured).
    const M = L.base.key === null ? 0 : Math.round(OVERSCAN * Math.max(W, H));
    let dpr = Math.min(DPR_MAX, window.devicePixelRatio || 1);
    const cw = W + 2 * M;
    const ch = H + 2 * M;
    if (cw * ch * dpr * dpr > MAX_PIXELS) dpr = Math.sqrt(MAX_PIXELS / (cw * ch));
    const fr = { view: { ...view }, scale, M, dpr, cw, ch, pw: Math.round(cw * dpr), ph: Math.round(ch * dpr), font: S.font };
    fr.key = `${content}|${view.x}|${view.y}|${view.w}|${view.h}|${dpr}|${fr.pw}|${fr.ph}|${M}`;
    return fr;
  }
  /** Size and place `ly` for frame `fr`. */
  function sizeLayer(ly, fr) {
    if (ly.w !== fr.pw || ly.h !== fr.ph) {
      ly.cv.width = ly.w = fr.pw;
      ly.cv.height = ly.h = fr.ph;
      ly.cv.style.width = fr.cw + 'px';
      ly.cv.style.height = fr.ch + 'px';
    }
    ly.cv.style.left = ly.cv.style.top = -fr.M + 'px';
  }
  /** A canvas never in the page, sized for `fr`. */
  function scratchFor(ly, fr) {
    if (ly.w !== fr.pw || ly.h !== fr.ph) {
      ly.cv.width = ly.w = fr.pw;
      ly.cv.height = ly.h = fr.ph;
    }
    return ctxOf(ly);
  }

  // ---- the big layers -----------------------------------------------------------------
  const scratch = { dimBack: layer('', false), dimNodes: layer('', true), dimLabels: layer('', true) };
  function drawBaseLayer(fr) {
    sizeLayer(L.base, fr);
    sc.drawBase(ctxOf(L.base), S, fr);
    L.base.key = fr.key;
  }
  function drawDimLayers(fr) {
    const back = scratchFor(scratch.dimBack, fr);
    const nodesCtx = scratchFor(scratch.dimNodes, fr);
    const labelsCtx = scratchFor(scratch.dimLabels, fr);
    sc.drawDimBack(back, S, fr);
    sc.drawDimNodes(nodesCtx, S, fr);
    sc.drawDimLabels(labelsCtx, S, fr, null);
    sizeLayer(L.dim, fr);
    sc.composeDim(ctxOf(L.dim), S, fr, scratch.dimBack.cv, scratch.dimLabels.cv, scratch.dimNodes.cv);
    L.dim.key = fr.key;
  }
  let idle = 0;
  /** Ready the faded layers for the first focus, when the page is idle. */
  function dimLater() {
    if (idle) return;
    idle = 1;
    afterPaint(() => {
      idle = 0;
      if (!view || !host.isConnected || plan || flat) return;
      const fr = frame();
      if (L.dim.key !== fr.key) drawDimLayers(fr);
    }, 400);
  }

  // ---- the patch ------------------------------------------------------------------------
  /** Layout units → device px of frame `fr`, outward, padded and clamped to the layer. */
  function toDevice(b, fr) {
    const k = fr.dpr * fr.scale;
    const ox = (fr.M - fr.view.x * fr.scale) * fr.dpr;
    const oy = (fr.M - fr.view.y * fr.scale) * fr.dpr;
    return [Math.max(0, Math.floor(b[0] * k + ox) - 3), Math.max(0, Math.floor(b[1] * k + oy) - 3), Math.min(fr.pw, Math.ceil(b[2] * k + ox) + 3), Math.min(fr.ph, Math.ceil(b[3] * k + oy) + 3)];
  }
  /** What the patch must cover, as a device rectangle of `fr` (null: nothing). */
  function litBounds(fr) {
    const b = [Infinity, Infinity, -Infinity, -Infinity];
    const add = (x0, y0, x1, y1) => {
      if (x0 < b[0]) b[0] = x0;
      if (y0 < b[1]) b[1] = y0;
      if (x1 > b[2]) b[2] = x1;
      if (y1 > b[3]) b[3] = y1;
    };
    const pad = 16 / scale;
    const disc = (i) => {
      const nd = nodes[i];
      const q = nd.r + 6 + pad;
      add(nd.x - q, nd.y - q, nd.x + q, nd.y + q);
    };
    if (plan) {
      for (const k of plan.edges) for (const [x, y] of (built[k] && built[k].pts) || []) add(x - pad, y - pad, x + pad, y + pad);
      for (const i of plan.on) {
        disc(i);
        const lp = lay.labels[i];
        if (lp) add(...sc.labelBox(S, i, lp.x, lp.y, lp.anchor));
      }
      for (const m of plan.moves) add(...sc.labelBox(S, m[0], m[1], m[2], m[3]));
      for (const st of S.stubs) if (st.i === plan.stubsOf) add(Math.min(st.x1, st.x2) - pad, Math.min(st.y1, st.y2) - pad, Math.max(st.x1, st.x2) + pad, Math.max(st.y1, st.y2) + pad);
    }
    tags.forEach((tag, j) => {
      // Every tag is drawn in `dim` too (unfaded): under a focus the patch
      // redraws them over its rectangle, but only a marked one makes it
      // bigger (all of them made it the whole box in an opened scope).
      if (j === pointerTag || j === focusTag) add(tag.tg.x - tag.tg.w / 2 - pad, tag.tg.y - tag.tg.h / 2 - pad, tag.tg.x + tag.tg.w / 2 + pad, tag.tg.y + tag.tg.h / 2 + pad);
    });
    for (const i of [pointerNode, curNode, focusNode]) if (i >= 0) disc(i);
    if (!(b[0] <= b[2])) return null;
    const r = toDevice(b, fr);
    return r[2] > r[0] && r[3] > r[1] ? r : null;
  }
  function ring(ctx, i, colour, width) {
    const nd = nodes[i];
    ctx.strokeStyle = colour;
    ctx.lineWidth = width;
    ctx.beginPath();
    ctx.arc(nd.x, nd.y, nd.r + 6, 0, 2 * Math.PI);
    ctx.stroke();
  }
  let labelScratch = null;
  /** The faded labels inside rectangle `r` of `fr` (all but `skip`), on a scratch canvas of r's size. */
  function fadedLabels(fr, r, skip) {
    const w = r[2] - r[0];
    const hh = r[3] - r[1];
    if (!labelScratch) {
      const c = document.createElement('canvas');
      labelScratch = { c, ctx: c.getContext('2d') };
    }
    const { c, ctx } = labelScratch;
    if (c.width < w || c.height < hh) {
      c.width = Math.max(c.width, w);
      c.height = Math.max(c.height, hh);
    } else {
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, w, hh);
    }
    viewTransform(ctx, fr, r);
    const items = [];
    for (const it of sc.baseLabels(S, fr)) {
      if (skip.has(it.i)) continue;
      const d = toDevice(sc.labelBox(S, it.i, it.x, it.y, it.anchor), fr);
      if (d[0] < r[2] && d[2] > r[0] && d[1] < r[3] && d[3] > r[1]) items.push(it);
    }
    sc.drawLabels(ctx, S, items);
    return c;
  }
  /** The frame's view transform, shifted to a patch at device rectangle `r`. */
  function viewTransform(ctx, fr, r) {
    const px = fr.dpr * fr.scale;
    ctx.setTransform(px, 0, 0, px, (fr.M - fr.view.x * fr.scale) * fr.dpr - r[0], (fr.M - fr.view.y * fr.scale) * fr.dpr - r[1]);
  }
  /** Draw the patch for frame `fr` (layers.md above), or hide it when nothing is marked. */
  function drawPatch(fr) {
    const P = L.patch;
    const r = !plan && pointerNode < 0 && curNode < 0 && focusNode < 0 && pointerTag < 0 && focusTag < 0 ? null : litBounds(fr);
    if (!r) {
      P.cv.hidden = true;
      P.rect = null;
      return;
    }
    const w = r[2] - r[0];
    const hh = r[3] - r[1];
    if (P.w !== w || P.h !== hh) {
      P.cv.width = P.w = w;
      P.cv.height = P.h = hh;
      P.cv.style.width = w / fr.dpr + 'px';
      P.cv.style.height = hh / fr.dpr + 'px';
    }
    P.cv.style.left = r[0] / fr.dpr - fr.M + 'px';
    P.cv.style.top = r[1] / fr.dpr - fr.M + 'px';
    P.rect = r;
    const ctx = ctxOf(P);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    // What is under it: with a focus, `dim` without the lit memories'
    // faded labels (each part at its layer's opacity, as `dim` is made);
    // else `base`.
    if (plan) {
      ctx.drawImage(scratch.dimBack.cv, r[0], r[1], w, hh, 0, 0, w, hh);
      ctx.globalAlpha = look.fadeLabel;
      ctx.drawImage(fadedLabels(fr, r, new Set(plan.on)), 0, 0, w, hh, 0, 0, w, hh);
      ctx.globalAlpha = look.fadeNode;
      ctx.drawImage(scratch.dimNodes.cv, r[0], r[1], w, hh, 0, 0, w, hh);
      ctx.globalAlpha = 1;
    } else ctx.drawImage(L.base.cv, r[0], r[1], w, hh, 0, 0, w, hh);
    viewTransform(ctx, fr, r);
    if (plan) {
      const recs = [];
      for (const k of plan.edges) if (want[k] && built[k] && built[k].rec) recs.push(built[k].rec);
      hooks.drawEdges(ctx, recs, S.T, { scale, zk: look.litEdge, alpha: 1 });
      if (plan.stubsOf >= 0) sc.drawStubs(ctx, S, fr, 'lit', plan.stubsOf);
      const moved = new Map(plan.moves.map((m) => [m[0], m]));
      const hidden = new Set(plan.nolbl);
      const items = [];
      for (const i of plan.on) {
        const lp = lay.labels[i];
        if (!lp || hidden.has(i)) continue;
        const m = moved.get(i);
        items.push({ i, x: m ? m[1] : lp.x, y: m ? m[2] : lp.y, anchor: m ? m[3] : lp.anchor, weight: plan.nb.has(i) ? look.labelWeight.nb : look.labelWeight.focus });
      }
      sc.drawLabels(ctx, S, items);
      const focused = new Set(plan.on.filter((i) => !plan.nb.has(i)));
      hooks.drawNodes(ctx, plan.on.map((i) => nodes[i]), S.T, { focused, version: -1 });
    }
    sc.drawTags(ctx, S, fr, pointerTag, focusTag, !plan);
    if (pointerNode >= 0) ring(ctx, pointerNode, S.T.ink3, 3);
    if (curNode >= 0) ring(ctx, curNode, S.T.focus, 3.4);
    if (focusNode >= 0) ring(ctx, focusNode, S.T.focus, 3.4);
    P.cv.hidden = false;
  }

  // ---- flush ------------------------------------------------------------------------------
  /** The patch (or the layers) changed since the last flush. */
  let dirty = true;
  let queued = false;
  /** Mid-gesture: the opaque layer the patch was drawn into (`gesture`). */
  let flat = false;
  function invalidate() {
    dirty = true;
    if (queued) return;
    queued = true;
    queueMicrotask(flush);
  }
  function flush() {
    queued = false;
    if (!view || !host.isConnected || flat) return;
    const fr = frame();
    const focus = !!plan;
    if (L.base.key !== fr.key) {
      const first = L.base.key === null;
      drawBaseLayer(fr);
      dirty = true;
      if (first) afterPaint(() => host.isConnected && !flat && invalidate(), 400);
      else if (!focus) dimLater();
      // The accessible list, once the first drawing is on screen (ensureList).
      if (!buttons && !listQueued) {
        listQueued = true;
        afterPaint(ensureList);
      }
    }
    if (focus && L.dim.key !== fr.key) {
      drawDimLayers(fr);
      dirty = true;
    }
    // With a focus the opaque faded copy covers the base: only it and the
    // patch are shown (every visible canvas is a compositor layer).
    if (L.dim.cv.hidden === focus) L.dim.cv.hidden = !focus;
    if (L.base.cv.hidden !== focus) L.base.cv.hidden = focus;
    if (dirty) {
      dirty = false;
      drawPatch(fr);
    }
  }

  // ---- light / dark ------------------------------------------------------------------
  const retheme = () => {
    if (!host.isConnected) {
      mo.disconnect();
      if (mq) mq.removeEventListener('change', retheme);
      return;
    }
    readTokens();
    invalidate();
  };
  const mo = new MutationObserver(retheme);
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-graph-style'] });
  const mq = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
  if (mq) mq.addEventListener('change', retheme);

  // ---- pointer: hover, cursor, click --------------------------------------------------
  let pointerNode = -1;
  let pointerEdge = -1;
  let pointerTag = -1;
  let focusNode = -1;
  let focusTag = -1;
  let curNode = -1;
  let cursor = '';
  /** What is under a client point: { node } | { edge } | { tag } | null, in the SVG's stacking order. */
  function hitAt(clientX, clientY) {
    if (!view) return null;
    const rect = host.getBoundingClientRect();
    const x = view.x + ((clientX - rect.left) / rect.width) * view.w;
    const y = view.y + ((clientY - rect.top) / rect.height) * view.h;
    for (let j = tags.length - 1; j >= 0; j--) {
      const tg = tags[j].tg;
      if (Math.abs(x - tg.x) <= tg.w / 2 && Math.abs(y - tg.y) <= tg.h / 2) return { tag: j };
    }
    const i = hitNode(x, y);
    if (i >= 0) return { node: i };
    if (!hitEdge) hitEdge = edgeGrid(built.map((b, k) => (b && want[k] ? b.pts : null)));
    const k = hitEdge(x, y, look.hitWidth / 2 / scale);
    return k >= 0 ? { edge: k } : null;
  }
  function setPointer(hit) {
    const node = hit && hit.node !== undefined ? hit.node : -1;
    const edgeK = hit && hit.edge !== undefined ? hit.edge : -1;
    const tag = hit && hit.tag !== undefined ? hit.tag : -1;
    const look2 = hit ? 'pointer' : '';
    if (look2 !== cursor) host.style.cursor = cursor = look2;
    if (node === pointerNode && edgeK === pointerEdge && tag === pointerTag) return;
    // A leave, then an enter, as the SVG's mouseout / mouseover fire them.
    if (pointerNode >= 0 || pointerEdge >= 0) cb.hover(null);
    pointerNode = node;
    pointerEdge = edgeK;
    pointerTag = tag;
    if (node >= 0) cb.hover({ node });
    else if (edgeK >= 0) cb.hover({ edge: edgeK });
    invalidate();
  }
  host.addEventListener('pointermove', (ev) => {
    // Mid-gesture the map moves under the pointer, and a drag is not a hover.
    if (cb.busy() || (ev.buttons && cb.dragged())) return;
    setPointer(hitAt(ev.clientX, ev.clientY));
  });
  host.addEventListener('pointerleave', () => setPointer(null));
  host.addEventListener('click', (ev) => {
    if (cb.dragged()) return;
    const hit = hitAt(ev.clientX, ev.clientY);
    if (hit && hit.tag !== undefined) tags[hit.tag].go();
    else if (hit) cb.pin(hit, false);
    else cb.unpin();
  });
  // The box never scrolls (overflow hidden): focusing a memory whose button
  // sits outside it must not shift the drawing.
  box.addEventListener('scroll', () => {
    if (box.scrollTop || box.scrollLeft) box.scrollTop = box.scrollLeft = 0;
  });

  // ---- keyboard and assistive technology, on the list -----------------------------------------
  const indexOf = (el) => (el && el.classList && el.classList.contains('g-a11y-node') ? Number(el.getAttribute('data-i')) : -1);
  const tagOf = (el) => (el && el.classList && el.classList.contains('g-a11y-tag') ? Number(el.getAttribute('data-j')) : -1);
  list.addEventListener('focusin', (ev) => {
    const i = indexOf(ev.target);
    const j = tagOf(ev.target);
    if (i >= 0) {
      focusNode = i;
      if (!cb.quiet()) cb.hover({ node: i });
    } else if (j >= 0) focusTag = j;
    invalidate();
  });
  list.addEventListener('focusout', (ev) => {
    const i = indexOf(ev.target);
    if (i >= 0) {
      focusNode = -1;
      cb.hover(null);
    } else if (tagOf(ev.target) >= 0) focusTag = -1;
    invalidate();
  });
  list.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Enter' && ev.key !== ' ') return;
    const i = indexOf(ev.target);
    const j = tagOf(ev.target);
    if (i < 0 && j < 0) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (i >= 0) cb.pin({ node: i }, true);
    else tags[j].go();
  });
  // A screen reader's activation is a click on the element.
  list.addEventListener('click', (ev) => {
    const i = indexOf(ev.target);
    const j = tagOf(ev.target);
    ev.stopPropagation();
    if (i >= 0) cb.pin({ node: i }, false);
    else if (j >= 0) tags[j].go();
  });


  return {
    el: host,
    pointerEl: host,
    composited: true,
    nodeIndexOf: indexOf,
    focusNode(i) {
      ensureList()[i].focus({ preventScroll: true });
    },
    setHub(i, on) {
      if (nodes[i].hub === on) return;
      nodes[i].hub = on;
      S.nodeVersion++;
      content++;
      invalidate();
    },
    setCur(i, on) {
      if (on) curNode = i;
      else if (curNode === i) curNode = -1;
      invalidate();
    },
    resetEdges() {},
    edge(k, e, pts, ends) {
      return { k, pts, rec: hooks.edge(e, pts, ends) };
    },
    placeEdges(b, w) {
      built = b;
      want = w;
      S.recs = [];
      for (let k = 0; k < b.length; k++) if (w[k] && b[k] && b[k].rec) S.recs.push(b[k].rec);
      hitEdge = null;
      content++;
      invalidate();
    },
    setStubs(next, many) {
      S.stubs = next;
      S.stubsMany = many;
      content++;
      invalidate();
    },
    setTags(next) {
      tags = next;
      for (const el of tagEls) el.remove();
      tagEls = tags.map((tag, j) => {
        const { tg } = tag;
        const el = h('div', { class: 'g-a11y-tag', role: 'link', tabindex: '0', 'data-j': j, 'aria-label': tag.aria });
        el.style.left = tg.x - tg.w / 2 + 'px';
        el.style.top = tg.y - tg.h / 2 + 'px';
        el.style.width = tg.w + 'px';
        el.style.height = tg.h + 'px';
        return el;
      });
      list.append(...tagEls);
      if (pointerTag >= tags.length) pointerTag = -1;
      if (focusTag >= tags.length) focusTag = -1;
      S.tags = tags.map((tag) => ({ tg: tag.tg, name: tag.name, count: tag.count, contradictions: tag.contradictions }));
      content++;
      invalidate();
    },
    show(v) {
      // The SVG's own arithmetic (a view box written to 0.1 unit, fitted
      // "meet" and centred), so both surfaces put a memory on the same pixel.
      const W = boxSize.w || 800;
      const H = boxSize.h || 500;
      const [vx, vy, vw, vh] = [v.x, v.y, v.w, v.h].map((n) => +n.toFixed(1));
      const s = Math.min(W / vw, H / vh);
      view = { x: vx - (W / s - vw) / 2, y: vy - (H / s - vh) / 2, w: W / s, h: H / s };
      scale = s;
      list.style.transform = `matrix(${s},0,0,${s},${-view.x * s},${-view.y * s})`;
      invalidate();
    },
    settled() {},
    gesture(on) {
      // Mid-gesture the layers move as one picture: the patch is drawn into
      // the opaque layer under it and only that one is shown. In software
      // compositing every visible canvas re-sends its whole backing store on
      // each frame of the gesture (5 layers at 2× DPR: a pan at 4× CPU
      // throttle fell to 23 fps).
      flush();
      const P = L.patch;
      if (on) {
        if (P.cv.hidden || !P.rect) return;
        const under = L.dim.cv.hidden ? L.base : L.dim;
        const ctx = ctxOf(under);
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.globalAlpha = 1;
        ctx.drawImage(P.cv, P.rect[0], P.rect[1]);
        P.cv.hidden = true;
        flat = under;
      } else if (flat) {
        // That layer now holds the patch: draw it afresh.
        flat.key = null;
        flat = false;
        invalidate();
      }
    },
    highlight(p) {
      plan = p;
      invalidate();
    },
    flush() {
      if (queued) flush();
    },
  };
}
