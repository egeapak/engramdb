// The canvas graph surface: the core host a canvas style draws through (a
// style with `renderer = 'canvas'` and a `canvas` hooks object; Plain's are
// themes/plain/canvas.js). Same members as the SVG surface
// (../surface-svg.js docs); views/graph.js cannot tell them apart except
// by `composited`.
//
// **What the host owns** (so a second canvas style supplies draw hooks
// only): the canvases and their sizing, the parts and layers below, the
// view transform, hit testing (./hit.js), the accessible list, the focus
// and hover rings, the stub lines and far-scope tags of an opened scope,
// the memory labels, token reading and the light/dark redraw. **What a
// style supplies** (`theme.canvas`):
//
//   look         { fadeNode, fadeEdge, fadeLabel, fadeBackdrop?, hitWidth,
//                  labelWeight: { focus, nb } }: its CSS's highlight numbers
//                  the core reads (the faded opacities of memories,
//                  relations, labels and backdrop, each applied to its part
//                  as a group, as the SVG fades a group; the relation click
//                  target in screen px; lit labels' weights). Anything else
//                  in it is the style's own (Plain's `litWidth`, read by its
//                  drawEdges when called with `lit`).
//   tokens(read) its colours, from custom properties (`read(name)`)
//   backdrop(g, lay)                  prepared once per graph
//   drawBelow(ctx, bd, T, fr), drawAbove(ctx, bd, T, fr)  under / over the relations
//   edge(e, pts, ends)                one relation's record (null: none)
//   edgeRank(rec)                     optional: where a record lies in the
//                                     drawing order, for the hit test
//   drawEdges(ctx, recs, T, { scale, lit })
//   drawNodes(ctx, recs, T, { focused, version })  recs: { i, n, x, y, r, hub }
//
// `fr` (the frame) is { scale, px: device px per layout unit, font }; `T`
// is the core tokens (bg, bg2, ink, ink2, ink3, red, redInk, focus) plus
// the style's. Every hook draws in layout units: the host has set the view
// transform. A hook keys any cache of its own by what it reads (Plain's
// textures by colour), so nothing tells it about a switch.
//
// **Parts and layers** (./scene.js draws them; each the view's box plus an
// overscan margin, at devicePixelRatio, capped). Six part bitmaps off the
// page (below, edges, above, labels, nodes, and over = above + labels +
// nodes), each with its own version and frame key, so a change redraws only
// its parts and the layers are blits. In the page, at most two canvases at
// once:
//   base      everything, unfaded: below, the relations drawn straight in,
//             over (so a display filter costs the relations and two blits);
//             at a new frame whose parts are behind (a settle), straight
//             from the scene, the parts following in idle time
//   dim       opaque, in place of `base` while something is focused: below,
//             edges, above, labels and nodes, each at its faded opacity as
//             a group, the tags unfaded
//   patch     a small canvas over either, just the rectangle the focus,
//             rings and marked tags cover: what is under it composed again
//             (with a focus, without the lit memories' faded labels), then
//             the lit memories, relations and labels, the tags, the rings
// Each canvas in the page is a compositor layer whose whole backing store
// is re-sent on every frame that changes it (software compositing), so a
// hover changes only the patch. A software blit is about 2.5 ms per
// million device pixels, so the hidden one of base / dim is brought up in
// idle time a part per callback, then the layer in bands of rows, each
// forced to raster then (`rasterNow`). A part or layer whose view only
// moved by whole device pixels at the same scale (a pan, the margin added
// after the first drawing) is copied at that offset and only the uncovered
// strips drawn, each under its own rectangle clip; a layer's strips come
// straight from the scene (`drawUnfaded`) for the unfaded one, so its pan
// copies one canvas, not every part (they follow when a compose needs
// them); the faded one's parts shift with it (drawing its strips from the
// scene through scratch parts measured slower on the big stores).
// `show` snaps a pan's view to whole device pixels so that this holds. A
// drawing built from copies is within antialiasing of one drawn whole,
// not bit-exact (bench/browser/checks.mjs bounds it).
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
// **Accessibility.** A never-hidden element over the drawing is
// `role="img"` with a label that counts the relations the filters show;
// every drawn memory is a `<button>` (the SVG `<g>`'s aria label,
// `tabindex=-1`, roving focus by ] / [) and every far-scope tag a
// `role="link"`, in an invisible list (opacity 0, no pointer events)
// placed over the drawing by one transform per view, so a screen magnifier
// following focus lands on the memory. The buttons are built in idle
// slices after the first paint, and at once when the view steps focus to
// one. Relations stay reachable through the relations table, as with the
// SVG surface (whose relations are aria-hidden too).

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
/** Accessible-list buttons built per idle slice at most. */
const LIST_SLICE = 200;
const CORE_TOKENS = { bg: '--bg', bg2: '--bg-2', ink: '--ink', ink2: '--ink-2', ink3: '--ink-3', red: '--red', redInk: '--red-ink', focus: '--focus' };
const PART_NAMES = Object.keys(sc.PARTS);

/** Whether this browser has what the host draws with. */
export function canvasSupported() {
  return typeof CanvasRenderingContext2D === 'function' && typeof Path2D === 'function' && typeof DOMMatrix === 'function' && 'addPath' in Path2D.prototype && 'roundRect' in CanvasRenderingContext2D.prototype;
}

/** Device pixels the hidden layer is composed in per idle callback (rows of them). */
const BAND_PX = 1.2e6;
const idleCall = (fn) => (typeof requestIdleCallback === 'function' ? requestIdleCallback(fn, { timeout: 500 }) : setTimeout(fn, 16));

/**
 * The surface, or null when a 2D context cannot be had (the view then
 * draws the style's SVG hooks). `o`: { g, lay, labels, theme, box (the
 * graph box), boxSize (kept current by the view), ariaLabel(relations
 * shown) → the picture's label, cb } (`cb` as for the SVG surface).
 */
export function canvasSurface(o) {
  const { g, lay, labels, theme, box, boxSize, cb } = o;
  const hooks = theme.canvas;
  const look = hooks.look;
  const layer = (cls, alpha) => {
    const cv = cls === null ? document.createElement('canvas') : h('canvas', { class: cls, 'aria-hidden': 'true' });
    return { cv, ctx: null, alpha, w: 0, h: 0, ver: null, fr: null, fk: null };
  };
  const L = { base: layer('g-cv-base', false), dim: layer('g-cv-dim', false), patch: layer('g-cv-patch', true) };
  L.base.ctx = L.base.cv.getContext('2d', { alpha: false });
  if (!L.base.ctx) return null;
  for (const k of ['base', 'dim', 'patch']) L[k].cv.hidden = true;
  const ctxOf = (ly) => ly.ctx || (ly.ctx = ly.cv.getContext('2d', { alpha: ly.alpha }));
  const parts = {};
  for (const name of PART_NAMES) parts[name] = layer(null, !sc.PARTS[name].opaque);
  const partCanvases = () => Object.fromEntries(PART_NAMES.map((n) => [n, parts[n].cv]));

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
  // What each part shows changed: tokens, relations, stubs, tags (memories: S.nodeVersion).
  let tokVer = 0;
  let edgeVer = 0;
  let stubVer = 0;
  let tagVer = 0;
  const partVer = (name) => (name === 'edges' ? `${tokVer}|${edgeVer}` : name === 'nodes' || name === 'over' ? `${tokVer}|${S.nodeVersion}` : `${tokVer}`);
  const layerVer = () => `${tokVer}|${edgeVer}|${S.nodeVersion}|${stubVer}|${tagVer}`;
  /** Once the first drawing is on screen: from then on every frame has the margin. */
  let warm = false;
  /** For the browser checks and bench: what was drawn, and how. */
  // (`ms`: the last bring-up of each layer, parts included, in ms; `partMs`:
  // the parts' share of it.)
  const stats = { full: 0, shifted: 0, composed: 0, ms: { base: 0, dim: 0 }, partMs: { base: 0, dim: 0 } };

  // ---- tokens ------------------------------------------------------------------
  function readTokens() {
    const cs = getComputedStyle(document.body);
    const read = (name) => cs.getPropertyValue(name).trim();
    const core = {};
    for (const [k, name] of Object.entries(CORE_TOKENS)) core[k] = read(name);
    sc.setTokens(S, { ...core, ...hooks.tokens(read) }, getComputedStyle(box).fontFamily || 'sans-serif');
    tokVer++;
  }
  readTokens();

  // ---- the DOM -------------------------------------------------------------------
  const pic = h('div', { class: 'g-cv-pic', role: 'img', 'aria-label': o.ariaLabel(g.edges.length) });
  const list = h('div', { class: 'g-a11y', role: 'group', 'aria-label': 'Memories' });
  // One button per memory, in index order, built in idle slices once the
  // first drawing is on screen (styling 800 of them showed in the first
  // paint, docs/contributors/dashboard-performance.md), or the rest at once
  // when the view moves focus to a memory. By hand rather than through
  // h(): the generic attribute walk showed too.
  let buttons = null;
  const made = [];
  let listQueued = false;
  function makeButtons(deadline) {
    const frag = document.createDocumentFragment();
    let n = 0;
    while (made.length < nodes.length && (!deadline || (n < LIST_SLICE && deadline.timeRemaining() > 1))) {
      const nd = nodes[made.length];
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
      made.push(b);
      frag.appendChild(b);
      n++;
    }
    list.insertBefore(frag, tagEls[0] || null);
    if (made.length === nodes.length) buttons = made;
  }
  function listSlice(deadline) {
    if (buttons || !alive()) return;
    makeButtons(deadline);
    if (!buttons) idleCall(listSlice);
  }
  function ensureList() {
    if (!buttons) makeButtons(null);
    return buttons;
  }
  let tagEls = [];
  const host = h('div', { class: 'graph g-canvas' }, pic, L.base.cv, L.dim.cv, L.patch.cv, list);
  const alive = () => host.isConnected;

  // ---- frames ----------------------------------------------------------------------
  function dprFor(cw, ch) {
    const dpr = Math.min(DPR_MAX, window.devicePixelRatio || 1);
    return cw * ch * dpr * dpr > MAX_PIXELS ? Math.sqrt(MAX_PIXELS / (cw * ch)) : dpr;
  }
  const marginFor = (W, H) => (warm ? Math.round(OVERSCAN * Math.max(W, H)) : 0);
  /** The frame for the box and view now: margin, ratio, device size (scene.js docs). */
  function frame() {
    const W = boxSize.w || 800;
    const H = boxSize.h || 500;
    // The first drawing is the box alone, the margin added once it is on
    // screen (copied into the centre, only the margin drawn).
    const M = marginFor(W, H);
    const cw = W + 2 * M;
    const ch = H + 2 * M;
    const dpr = dprFor(cw, ch);
    const fr = { view: { ...view }, scale, M, dpr, cw, ch, pw: Math.round(cw * dpr), ph: Math.round(ch * dpr), font: S.font };
    fr.key = `${view.x}|${view.y}|${view.w}|${view.h}|${scale}|${dpr}|${fr.pw}|${fr.ph}|${M}`;
    return fr;
  }
  /**
   * The whole-device-pixel offset that carries a drawing of frame `a` to
   * frame `b` (same scale and ratio), or null when there is none.
   */
  function shiftOf(a, b) {
    if (a.scale !== b.scale || a.dpr !== b.dpr) return null;
    const dx = (b.M - b.view.x * b.scale - (a.M - a.view.x * a.scale)) * b.dpr;
    const dy = (b.M - b.view.y * b.scale - (a.M - a.view.y * a.scale)) * b.dpr;
    const rx = Math.round(dx);
    const ry = Math.round(dy);
    if (Math.abs(dx - rx) > 1e-3 || Math.abs(dy - ry) > 1e-3) return null;
    if (rx >= b.pw || -rx >= a.pw || ry >= b.ph || -ry >= a.ph) return null;
    return [rx, ry];
  }
  /**
   * The device rectangles [x0, y0, x1, y1] of frame `fr` that a drawing of
   * frame `old` copied at offset `sh` leaves uncovered: up to four bands.
   */
  function strips(fr, sh, old) {
    const x0 = Math.max(0, sh[0]);
    const y0 = Math.max(0, sh[1]);
    const x1 = Math.min(fr.pw, sh[0] + old.pw);
    const y1 = Math.min(fr.ph, sh[1] + old.ph);
    const out = [];
    if (y0 > 0) out.push([0, 0, fr.pw, y0]);
    if (y1 < fr.ph) out.push([0, y1, fr.pw, fr.ph]);
    if (x0 > 0) out.push([0, y0, x0, y1]);
    if (x1 < fr.pw) out.push([x1, y0, fr.pw, y1]);
    return out;
  }
  let spare = null;
  /** Size `ly` for `fr` (placed in the page when `inPage`), keeping its pixels at offset `sh` when given. */
  function sizeLayer(ly, fr, inPage, sh) {
    if (ly.w !== fr.pw || ly.h !== fr.ph) {
      if (sh) {
        // A resize clears a canvas: its pixels go through a spare first.
        spare = spare || document.createElement('canvas');
        spare.width = ly.w;
        spare.height = ly.h;
        spare.getContext('2d').drawImage(ly.cv, 0, 0);
      }
      ly.cv.width = ly.w = fr.pw;
      ly.cv.height = ly.h = fr.ph;
      if (inPage) {
        ly.cv.style.width = fr.cw + 'px';
        ly.cv.style.height = fr.ch + 'px';
      }
      if (sh) {
        const ctx = ctxOf(ly);
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.globalAlpha = 1;
        ctx.drawImage(spare, sh[0], sh[1]);
        spare.width = spare.height = 0;
      }
    } else if (sh) {
      const ctx = ctxOf(ly);
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.globalAlpha = 1;
      ctx.globalCompositeOperation = 'copy';
      ctx.drawImage(ly.cv, sh[0], sh[1]);
      ctx.globalCompositeOperation = 'source-over';
    }
    if (inPage) ly.cv.style.left = ly.cv.style.top = -fr.M + 'px';
  }
  /**
   * Bring `ly` to frame `fr` showing version `ver`: nothing when it does,
   * a copy and the uncovered strips when only the view moved by whole
   * pixels, else the whole of `draw(ctx, fr)`.
   */
  function ensure(ly, fr, ver, draw, inPage) {
    if (ly.ver === ver && ly.fk === fr.key) return false;
    const old = ly.fr;
    const sh = ly.ver === ver && old ? shiftOf(old, fr) : null;
    sizeLayer(ly, fr, inPage, sh);
    const ctx = ctxOf(ly);
    if (sh) {
      // Each uncovered strip under its own rectangle clip: whole device
      // pixels, so the browser clips by rectangle (no mask, and a blit
      // reads only the strip; an even-odd clip of both rectangles cost
      // 90 ms a pan at ratio 2).
      for (const r of strips(fr, sh, old)) {
        ctx.save();
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.beginPath();
        ctx.rect(r[0], r[1], r[2] - r[0], r[3] - r[1]);
        ctx.clip();
        draw(ctx, fr);
        ctx.restore();
      }
      stats.shifted++;
    } else {
      draw(ctx, fr);
      stats.full++;
    }
    ly.ver = ver;
    ly.fr = fr;
    ly.fk = fr.key;
    return true;
  }
  /** Whether every part of layer `which` (base / dim) shows frame `fr` now. */
  const partsAt = (which, fr) => sc.LAYER_PARTS[which].every((n) => parts[n].ver === partVer(n) && parts[n].fk === fr.key);
  const ensurePart = (name, fr) => ensure(parts[name], fr, partVer(name), (c, f) => sc.drawPart(c, S, f, name), false);
  /** Compose layer `ly` (base, or `faded`: dim) for `fr`, its parts brought up to date first. */
  function ensureLayer(ly, fr, faded) {
    const ver = layerVer();
    if (ly.ver === ver && ly.fk === fr.key) return false;
    const t0 = performance.now();
    // A pan of the unfaded layer: a copy and its strips drawn straight from
    // the scene. Its parts stay where they were and follow when a compose
    // needs them (each would cost a copy now).
    // (Faded, the parts shift too and the strips compose from them: drawn
    // from the scene through scratch parts it measured slower on the big
    // stores, s2k / s10k at DPR 2: pan settle 59 → 88 ms.)
    if (!faded && ly.ver === ver && ly.fr && shiftOf(ly.fr, fr)) {
      const did = ensure(ly, fr, ver, (c, f) => sc.drawUnfaded(c, S, f), true);
      stats.ms.base = performance.now() - t0;
      stats.partMs.base = 0;
      return did;
    }
    // The unfaded layer at a new frame with its parts behind too (a settle
    // at a new scale): drawn straight from the scene, which spares two
    // full-size blits; its parts follow in idle time, for the next filter.
    // (Same frame, new versions — a filter, a theme — compose: measured
    // the cheaper there, and a filter undone then gives the same pixels.)
    if (!faded && ly.fk !== fr.key && !partsAt('base', fr)) {
      const did = ensure(ly, fr, ver, (c, f) => sc.drawUnfaded(c, S, f), true);
      stats.ms.base = performance.now() - t0;
      stats.partMs.base = 0;
      return did;
    }
    for (const name of sc.LAYER_PARTS[faded ? 'dim' : 'base']) ensurePart(name, fr);
    const t1 = performance.now();
    stats.composed++;
    const did = ensure(ly, fr, ver, (c, f) => sc.compose(c, S, f, partCanvases(), faded), true);
    const k = faded ? 'dim' : 'base';
    stats.ms[k] = performance.now() - t0;
    stats.partMs[k] = t1 - t0;
    return did;
  }
  let idleFor = false;
  /** The hidden layer's compose in progress: { ly, key, y (the next row) }. */
  let band = null;
  // A 1 × 1 canvas to draw a finished layer into: that makes the browser
  // raster the layer now, in idle time, rather than at the paint that first
  // shows it (a canvas records its drawing until something needs pixels).
  let sink = null;
  const rasterNow = (cv) => {
    sink = sink || document.createElement('canvas');
    sink.width = sink.height = 1;
    sink.getContext('2d').drawImage(cv, 0, 0, 1, 1, 0, 0, 1, 1);
  };
  /**
   * Compose the hidden one of base / dim in idle time, so a focus (or its
   * end) finds it ready: one part per idle callback, then the layer in
   * bands of rows, so no step is a long task.
   */
  function otherLater() {
    if (idleFor) return;
    idleFor = true;
    const step = () => {
      idleFor = false;
      if (!view || !alive() || flat) return;
      const focus = !!plan;
      const fr = frame();
      const faded = !focus;
      // The shown layer's parts come first, one per callback, when a pan or
      // a direct drawing left them behind: under a focus the patch
      // composes from the faded ones, without one a filter from the base's.
      const own = focus ? 'dim' : 'base';
      if (!partsAt(own, fr)) {
        for (const name of sc.LAYER_PARTS[own]) {
          if (ensurePart(name, fr)) {
            rasterNow(parts[name].cv);
            break;
          }
        }
        idleFor = true;
        idleCall(step);
        return;
      }
      const ly = focus ? L.base : L.dim;
      const ver = layerVer();
      if (ly.ver === ver && ly.fk === fr.key) return;
      // A copy and strips (a pan) are cheap: at once, no part needed. A
      // whole compose is a blit per part over every device pixel (about
      // 2.5 ms a million in software raster): first the parts, one per
      // idle callback, then the layer in bands of rows, one per callback.
      if (ly.ver === ver && ly.fr && shiftOf(ly.fr, fr)) {
        if (ensureLayer(ly, fr, faded)) rasterNow(ly.cv);
        return;
      }
      for (const name of sc.LAYER_PARTS[faded ? 'dim' : 'base']) {
        if (ensurePart(name, fr)) {
          rasterNow(parts[name].cv);
          idleFor = true;
          idleCall(step);
          return;
        }
      }
      const key = `${ver}|${fr.key}|${faded}`;
      if (!band || band.ly !== ly || band.key !== key) {
        band = { ly, key, y: 0 };
        sizeLayer(ly, fr, true, null);
        ly.ver = null;
      }
      const rows = Math.max(1, Math.ceil(BAND_PX / fr.pw));
      const ctx = ctxOf(ly);
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.beginPath();
      ctx.rect(0, band.y, fr.pw, rows);
      ctx.clip();
      sc.compose(ctx, S, fr, partCanvases(), faded);
      ctx.restore();
      rasterNow(ly.cv);
      band.y += rows;
      if (band.y < fr.ph) {
        idleFor = true;
        idleCall(step);
        return;
      }
      band = null;
      ly.ver = ver;
      ly.fr = fr;
      ly.fk = fr.key;
      stats.composed++;
      stats.full++;
    };
    afterPaint(step, 400);
  }

  // ---- the patch ------------------------------------------------------------------------
  /** Layout units → device px of frame `fr`, outward, padded and clamped to the layer. */
  function toDevice(b, fr) {
    const k = fr.dpr * fr.scale;
    const ox = (fr.M - fr.view.x * fr.scale) * fr.dpr;
    const oy = (fr.M - fr.view.y * fr.scale) * fr.dpr;
    return [Math.max(0, Math.floor(b[0] * k + ox) - 3), Math.max(0, Math.floor(b[1] * k + oy) - 3), Math.min(fr.pw, Math.ceil(b[2] * k + ox) + 3), Math.min(fr.ph, Math.ceil(b[3] * k + oy) + 3)];
  }
  const weightOf = (i) => (plan && plan.nb.has(i) ? look.labelWeight.nb : look.labelWeight.focus);
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
        // The faded label under it is covered too (its weight is the base's).
        if (lp) {
          add(...sc.labelBox(S, i, lp.x, lp.y, lp.anchor, weightOf(i)));
          add(...sc.labelBox(S, i, lp.x, lp.y, lp.anchor, 600));
        }
      }
      for (const m of plan.moves) add(...sc.labelBox(S, m[0], m[1], m[2], m[3], weightOf(m[0])));
      for (const st of S.stubs) if (st.i === plan.stubsOf) add(Math.min(st.x1, st.x2) - pad, Math.min(st.y1, st.y2) - pad, Math.max(st.x1, st.x2) + pad, Math.max(st.y1, st.y2) + pad);
    }
    // Every tag is in the layers too: only a marked one widens the patch
    // (all of them made it the whole box in an opened scope).
    tags.forEach((tag, j) => {
      if (j === pointerTag || j === focusTag) add(tag.tg.x - tag.tg.w / 2 - pad, tag.tg.y - tag.tg.h / 2 - pad, tag.tg.x + tag.tg.w / 2 + pad, tag.tg.y + tag.tg.h / 2 + pad);
    });
    for (const i of [pointerNode, curNode, focusNode]) if (i >= 0) disc(i);
    if (!(b[0] <= b[2])) return null;
    const r = toDevice(b, fr);
    // On whole CSS pixels: a patch at a half pixel (an odd device corner at
    // 2x) is resampled by the compositor, so it would not match the layer
    // it is drawn into mid-gesture.
    const d = fr.dpr;
    r[0] = Math.round(Math.floor(r[0] / d) * d);
    r[1] = Math.round(Math.floor(r[1] / d) * d);
    r[2] = Math.min(fr.pw, Math.round(Math.ceil(r[2] / d) * d));
    r[3] = Math.min(fr.ph, Math.round(Math.ceil(r[3] / d) * d));
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
  /** The faded labels inside rectangle `r` of `fr` (all but `skip`), on a scratch canvas at r's corner. */
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
    sc.viewTransform(ctx, fr, r[0], r[1]);
    const items = [];
    for (const it of sc.baseLabels(S, fr)) {
      if (skip.has(it.i)) continue;
      const d = toDevice(sc.labelBox(S, it.i, it.x, it.y, it.anchor, 600), fr);
      if (d[0] < r[2] && d[2] > r[0] && d[1] < r[3] && d[3] > r[1]) items.push(it);
    }
    sc.drawLabels(ctx, S, items);
    return c;
  }
  /** Draw the patch for frame `fr` (layers above), or hide it when nothing is marked. */
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
    // The canvas only grows (to the largest patch at this ratio): a resize
    // reallocates it and makes the next font set re-resolve its style.
    if (P.w < w || P.h < hh || P.dpr !== fr.dpr) {
      P.cv.width = P.w = Math.max(P.dpr === fr.dpr ? P.w : 0, w);
      P.cv.height = P.h = Math.max(P.dpr === fr.dpr ? P.h : 0, hh);
      P.dpr = fr.dpr;
      P.cv.style.width = P.w / fr.dpr + 'px';
      P.cv.style.height = P.h / fr.dpr + 'px';
    }
    P.cv.style.left = r[0] / fr.dpr - fr.M + 'px';
    P.cv.style.top = r[1] / fr.dpr - fr.M + 'px';
    P.rect = r;
    const ctx = ctxOf(P);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.clearRect(0, 0, P.w, P.h);
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, w, hh);
    ctx.clip();
    // What is under it: with a focus, `dim` without the lit memories'
    // faded labels; else `base`.
    if (plan) {
      const lbl = fadedLabels(fr, r, new Set(plan.on));
      // (The faded parts are current whenever dim is: it is composed from
      // them, a pan of it included. Brought up here should that ever not
      // hold.)
      for (const name of sc.LAYER_PARTS.dim) ensurePart(name, fr);
      sc.compose(ctx, S, fr, partCanvases(), true, r[0], r[1], lbl);
    }
    else ctx.drawImage(L.base.cv, -r[0], -r[1]);
    sc.viewTransform(ctx, fr, r[0], r[1]);
    if (plan) {
      const recs = [];
      for (const k of plan.edges) if (want[k] && built[k] && built[k].rec) recs.push(built[k].rec);
      hooks.drawEdges(ctx, recs, S.T, { scale, lit: true });
      if (plan.stubsOf >= 0) sc.drawStubs(ctx, S, fr, 'lit', plan.stubsOf);
      const moved = new Map(plan.moves.map((m) => [m[0], m]));
      const hidden = new Set(plan.nolbl);
      const items = [];
      for (const i of plan.on) {
        const lp = lay.labels[i];
        if (!lp || hidden.has(i)) continue;
        const m = moved.get(i);
        items.push({ i, x: m ? m[1] : lp.x, y: m ? m[2] : lp.y, anchor: m ? m[3] : lp.anchor, weight: weightOf(i) });
      }
      sc.drawLabels(ctx, S, items);
      const focused = new Set(plan.on.filter((i) => !plan.nb.has(i)));
      hooks.drawNodes(ctx, plan.on.map((i) => nodes[i]), S.T, { focused, version: -1 });
    }
    sc.drawTags(ctx, S, fr, pointerTag, focusTag, !plan);
    if (pointerNode >= 0) ring(ctx, pointerNode, S.T.ink3, 3);
    if (curNode >= 0) ring(ctx, curNode, S.T.focus, 3.4);
    if (focusNode >= 0) ring(ctx, focusNode, S.T.focus, 3.4);
    ctx.restore();
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
  // Set by the first drawing, never cleared: a flattened gesture (which
  // marks the layers stale) must not take the first-drawing path again.
  let drawnOnce = false;
  function flush() {
    queued = false;
    if (!view || !alive() || flat) return;
    const fr = frame();
    const focus = !!plan;
    // Only the shown one of base / dim is brought up to date now; the
    // other follows in idle time.
    if (ensureLayer(focus ? L.dim : L.base, fr, focus)) {
      dirty = true;
      if (!drawnOnce) {
        drawnOnce = true;
        afterPaint(() => {
          if (!alive()) return;
          warm = true;
          invalidate();
        }, 400);
        if (!listQueued) {
          listQueued = true;
          afterPaint(listSlice);
        }
      } else otherLater();
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

  // ---- light / dark, device pixel ratio ----------------------------------------------
  const retheme = () => {
    if (!alive()) {
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
  // A move to a screen of another ratio changes no CSS size, so no resize
  // observer fires: a resolution query does, re-armed at each new ratio.
  function watchRatio() {
    if (!window.matchMedia) return;
    const q = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
    q.addEventListener(
      'change',
      () => {
        if (!alive()) return;
        watchRatio();
        invalidate();
      },
      { once: true },
    );
  }
  watchRatio();

  // ---- pointer: hover, cursor, click --------------------------------------------------
  let pointerNode = -1;
  let pointerEdge = -1;
  let pointerTag = -1;
  let focusNode = -1;
  let focusTag = -1;
  let curNode = -1;
  let cursor = '';
  /** What is under a client point: { node } | { edge } | { tag } | null, in the drawing's stacking order. */
  function hitAt(clientX, clientY) {
    if (!view) return null;
    // One factor for both axes: the drawing is scaled uniformly (by the
    // view, and mid-gesture by the pane's transform), and the host's width
    // is the box's, which the view keeps `view` fitted to.
    const rect = host.getBoundingClientRect();
    const k = view.w / (rect.width || 1);
    const x = view.x + (clientX - rect.left) * k;
    const y = view.y + (clientY - rect.top) * k;
    for (let j = tags.length - 1; j >= 0; j--) {
      const tg = tags[j].tg;
      if (Math.abs(x - tg.x) <= tg.w / 2 && Math.abs(y - tg.y) <= tg.h / 2) return { tag: j };
    }
    const i = hitNode(x, y);
    if (i >= 0) return { node: i };
    if (!hitEdge) {
      const shown = built.map((b, kk) => (b && want[kk] && b.rec ? b : null));
      hitEdge = edgeGrid(
        shown.map((b) => (b ? b.pts : null)),
        hooks.edgeRank ? shown.map((b) => (b ? hooks.edgeRank(b.rec) : 0)) : null,
      );
    }
    const e = hitEdge(x, y, look.hitWidth / 2 / scale);
    return e >= 0 ? { edge: e } : null;
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

  host.graphDebug = {
    stats,
    /** Device bytes of every canvas the host holds now (backing stores, 4 per pixel). */
    backingBytes: () => [L.base, L.dim, L.patch, ...Object.values(parts)].reduce((a, ly) => a + 4 * ly.cv.width * ly.cv.height, 0) + (labelScratch ? 4 * labelScratch.c.width * labelScratch.c.height : 0),
    relationsDrawn: () => S.recs.length,
    listBuilt: () => made.length,
    listDone: () => !!buttons,
    parts: () => parts,
    /** Mark every part and layer stale, so the next flush draws each one whole. */
    redrawAll: () => {
      for (const ly of [L.base, L.dim, ...Object.values(parts)]) ly.ver = 'stale';
      invalidate();
    },
  };

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
      let shown = 0;
      for (let k = 0; k < b.length; k++) {
        if (!w[k]) continue;
        shown++;
        if (b[k] && b[k].rec) S.recs.push(b[k].rec);
      }
      pic.setAttribute('aria-label', o.ariaLabel(shown));
      hitEdge = null;
      edgeVer++;
      invalidate();
    },
    setStubs(next, many) {
      S.stubs = next;
      S.stubsMany = many;
      stubVer++;
      invalidate();
    },
    setTags(next) {
      // Marks follow a tag by name across a rebuild (a filter can drop an
      // earlier one); focus moves to the rebuilt element of the same name.
      const nameOf = (j) => (j >= 0 && tags[j] ? tags[j].name : null);
      const hotName = nameOf(pointerTag);
      const focusName = nameOf(focusTag);
      const hadFocus = focusTag >= 0 && tagEls[focusTag] === document.activeElement;
      tags = next;
      const at = (name) => (name === null ? -1 : tags.findIndex((t) => t.name === name));
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
      pointerTag = at(hotName);
      focusTag = at(focusName);
      if (hadFocus) {
        if (focusTag >= 0) tagEls[focusTag].focus({ preventScroll: true });
        else box.focus({ preventScroll: true });
      }
      S.tags = tags.map((tag) => ({ tg: tag.tg, name: tag.name, count: tag.count, contradictions: tag.contradictions }));
      tagVer++;
      invalidate();
    },
    show(v) {
      // The SVG's own arithmetic (a view box written to 0.1 unit, fitted
      // "meet" and centred), so both surfaces put a memory on the same pixel.
      const W = boxSize.w || 800;
      const H = boxSize.h || 500;
      const [vx, vy, vw, vh] = [v.x, v.y, v.w, v.h].map((n) => +n.toFixed(1));
      const s = Math.min(W / vw, H / vh);
      const next = { x: vx - (W / s - vw) / 2, y: vy - (H / s - vh) / 2, w: W / s, h: H / s };
      // A pan (same scale and box): moved by whole device pixels, so the
      // drawing can be copied and only the uncovered strips drawn (under
      // half a device pixel from the SVG's placement).
      if (view && s === scale && Math.abs(next.w - view.w) < 1e-9 && Math.abs(next.h - view.h) < 1e-9) {
        const M = marginFor(W, H);
        const k = s * dprFor(W + 2 * M, H + 2 * M);
        next.x = view.x + Math.round((next.x - view.x) * k) / k;
        next.y = view.y + Math.round((next.y - view.y) * k) / k;
      }
      view = next;
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
        ctx.drawImage(P.cv, 0, 0, P.rect[2] - P.rect[0], P.rect[3] - P.rect[1], P.rect[0], P.rect[1], P.rect[2] - P.rect[0], P.rect[3] - P.rect[1]);
        P.cv.hidden = true;
        flat = under;
      } else if (flat) {
        // That layer now holds the patch: composed afresh (from the parts,
        // which are still current) at the settle.
        flat.ver = 'stale';
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
