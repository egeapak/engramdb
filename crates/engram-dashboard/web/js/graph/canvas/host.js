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
// overscan margin, at devicePixelRatio). `LAYER_PX` bounds the margin:
// it shrinks so box + margin stay within it. The box itself is bounded by
// the page's CSS (`.wrap`'s 1240 px less the 300 px card, or the whole
// width under 980 px; min(70vh, 720px) tall), so it is at most ~950 × 720
// CSS px, 2.7 M device px at DPR 2: under LAYER_PX, which then holds as a
// cap. At DPR 2 the margin is what is left, 20–60 px (measured: 61 px on a
// 914 × 600 box, 27 on 914 × 718, 20 on 946 × 718; ~57 MB of canvases on
// each), at DPR 1 the full OVERSCAN. A box over LAYER_PX on its own (a
// stylesheet change) would get no margin, bounded by `MAX_PIXELS` only.
// Three part bitmaps
// off the page, each with its own version and frame key: `below` (the
// opaque backdrop), `over` (above + labels + memories, unfaded) and `faint`
// (labels and memories, each faded as a group); the relations, stubs and
// tags, which a display filter changes, are in no part. In the page, at
// most two canvases at once:
//   base      everything, unfaded: below, stubs and relations drawn straight
//             in, over, tags (so a display filter costs the relations and
//             two blits); from the scene where its parts are behind
//   dim       opaque, in place of `base` while something is focused or
//             hovered: below, the relations faded as a group, above, faint
//             (so a filter under a hover costs the relations and three blits)
//   patch     a small canvas over either, just the rectangle the focus,
//             rings and marked tags cover: a copy of the layer under it
//             (with a focus, the lit memories' label boxes drawn again
//             without their faded labels), then the lit memories, relations
//             and labels, the tags, the rings
// Every layer and part keeps `pend`, the device rectangles it still owes
// for its version and frame. `bring` moves one to a new frame: when only
// the view moved by whole device pixels at the same version (a pan) its
// pixels are copied along and the uncovered strips owed, else all of it;
// `settle` draws what it owes inside a region, in one pass under a union
// clip. A flush settles the shown layer's box only; the margin, the parts
// and the hidden one of base / dim are drawn in idle time (`pump`), parts
// first, in bands of rows sized from each kind's measured cost per pixel,
// each forced to raster then (`rasterNow`: a canvas records until
// something needs its pixels) and at most `ROOM_MS` per callback, so no
// idle task is a long one; a kind's first band (its backing store's
// allocation) runs alone. A browser that grants no idle period (headless
// software compositing) runs the rest in background-priority tasks.
// Memories whose look a filter changed (`setHub`) are owed as their discs
// a frame later, by every layer and part that draws memories, and drawn
// with only what reaches them. Each canvas in the page is a compositor
// layer whose whole backing store is re-sent on every frame that changes it
// (software compositing), so a hover changes only the patch. A drawing
// built from copies and rectangles is within antialiasing of one drawn
// whole, not bit-exact (bench/browser/checks.mjs bounds it).
//
// (A raster worker drawing these layers off the page's thread was built
// and measured: it lost the first paint, 96 against 53 ms at 1k memories,
// docs/contributors/dashboard-performance.md.)
//
// **Zoom and pan.** The view's composited gesture moves the canvases (CSS
// transform on `.g-pane`) and nothing is drawn until it settles (`gesture`
// first draws the patch into the layer under it, so one canvas moves); the
// overscan margin is what a pan or zoom-out shows before then (the first
// drawing has it in its frame, owed to idle time). A settle
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
const LIST_SLICE = 60;
const CORE_TOKENS = { bg: '--bg', bg2: '--bg-2', ink: '--ink', ink2: '--ink-2', ink3: '--ink-3', red: '--red', redInk: '--red-ink', focus: '--focus' };

/** Whether this browser has what the host draws with. */
export function canvasSupported() {
  return typeof CanvasRenderingContext2D === 'function' && typeof Path2D === 'function' && typeof DOMMatrix === 'function' && 'addPath' in Path2D.prototype && 'roundRect' in CanvasRenderingContext2D.prototype;
}

/**
 * The margin keeps each layer (box + margin) within this many device
 * pixels, five full-size canvases being kept (base, dim and the three
 * parts); a box over it alone gets no margin and is bounded by MAX_PIXELS
 * (module docs).
 */
const LAYER_PX = 3e6;
/** Idle work: one step aims at this many ms, in bands of rows within these bounds (device px). */
const STEP_MS = 12;
/** At most this many ms of idle work per idle callback (a task over 50 ms is a long task). */
const ROOM_MS = 30;
const MIN_BAND_PX = 4e4;
const MAX_BAND_PX = 1.5e6;
/** Owed rectangles a layer keeps apart before they are merged into their bounds. */
const PEND_MAX = 400;
/** Rectangles under this share of the layer draw only the memories and labels that reach them. */
const FEW_SHARE = 0.15;
/** A scratch canvas over this many device pixels is let go of after each drawing. */
const SCRATCH_KEEP = 1e6;
const idleCall = (fn) => (typeof requestIdleCallback === 'function' ? requestIdleCallback(fn, { timeout: 500 }) : setTimeout(fn, 16));
// A browser that grants no idle period while nothing changes on screen
// (headless software compositing does not) runs an idle callback only at
// its timeout: the owed drawing then goes on in background-priority tasks
// of one step each, which still yield to input and frames.
const bgCall = (fn) => (typeof scheduler === 'object' && scheduler && typeof scheduler.postTask === 'function' ? scheduler.postTask(() => fn(null), { priority: 'background' }) : setTimeout(() => fn(null), 0));

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
  const layer = (name, cls, alpha) => {
    const inPage = cls !== null;
    const cv = inPage ? h('canvas', { class: cls, 'aria-hidden': 'true' }) : document.createElement('canvas');
    return { name, cv, ctx: null, alpha, inPage, w: 0, h: 0, ver: null, fr: null, fk: null, pend: [], ms: 0, draw: null };
  };
  const L = { base: layer('base', 'g-cv-base', false), dim: layer('dim', 'g-cv-dim', false), patch: layer('patch', 'g-cv-patch', true) };
  L.base.ctx = L.base.cv.getContext('2d', { alpha: false });
  if (!L.base.ctx) return null;
  for (const k of ['base', 'dim', 'patch']) L[k].cv.hidden = true;
  const ctxOf = (ly) => ly.ctx || (ly.ctx = ly.cv.getContext('2d', { alpha: ly.alpha }));
  // The part bitmaps (scene.js PARTS), off the page.
  const B = layer('below', null, false);
  const O = layer('over', null, true);
  const F = layer('faint', null, true);

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
  // What the layers show changed: tokens, relations, stubs, tags (memories:
  // `setHub` owes their discs instead).
  let tokVer = 0;
  let edgeVer = 0;
  let stubVer = 0;
  let tagVer = 0;
  /** Once the first drawing is on screen: from then on idle work is scheduled. */
  let warm = false;
  /** For the browser checks and bench: what was drawn, and how. */
  // (`ms`: the time spent bringing each layer to its latest frame, idle
  // bands included; `partMs` is kept at 0 for the bench's older readers.)
  const stats = { full: 0, shifted: 0, composed: 0, owed: 0, ms: { base: 0, dim: 0 }, partMs: { base: 0, dim: 0 } };

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
  // Gone once it was in the page and is no longer: `dispose` then lets go
  // of everything (a host left behind must not keep its canvases).
  let placed = false;
  let disposed = false;
  const alive = () => {
    if (disposed) return false;
    if (host.isConnected) return (placed = true);
    if (placed) dispose();
    return false;
  };

  // ---- frames ----------------------------------------------------------------------
  function dprFor(cw, ch) {
    const dpr = Math.min(DPR_MAX, window.devicePixelRatio || 1);
    return cw * ch * dpr * dpr > MAX_PIXELS ? Math.sqrt(MAX_PIXELS / (cw * ch)) : dpr;
  }
  /**
   * The overscan margin (CSS px, each side) for a W × H box: OVERSCAN of its
   * larger side, cut so box + margin hold at most LAYER_PX device pixels
   * (five full-size canvases are kept, docs at the top); none when the box
   * alone is over that (no box the page's CSS allows is: module docs).
   */
  function marginFor(W, H) {
    const d = Math.min(DPR_MAX, window.devicePixelRatio || 1);
    const A = LAYER_PX / (d * d);
    if (W * H >= A) return 0;
    const fit = (-(W + H) + Math.sqrt((W + H) * (W + H) - 4 * (W * H - A))) / 4;
    return Math.max(0, Math.floor(Math.min(OVERSCAN * Math.max(W, H), fit)));
  }
  /** The frame for the box and view now: margin, ratio, device size (scene.js docs). */
  function frame() {
    const W = boxSize.w || 800;
    const H = boxSize.h || 500;
    // (The margin is in the frame from the first drawing on, which draws
    // the box alone: the margin is owed, for idle time after the paint.)
    const M = marginFor(W, H);
    const cw = W + 2 * M;
    const ch = H + 2 * M;
    const dpr = dprFor(cw, ch);
    const fr = { view: { ...view }, scale, M, dpr, cw, ch, pw: Math.round(cw * dpr), ph: Math.round(ch * dpr), font: S.font };
    fr.key = `${view.x}|${view.y}|${view.w}|${view.h}|${scale}|${dpr}|${fr.pw}|${fr.ph}|${M}`;
    return fr;
  }
  /** The box (what is on screen) as a device rectangle of `fr`. */
  const boxRect = (fr) => [Math.floor(fr.M * fr.dpr), Math.floor(fr.M * fr.dpr), Math.min(fr.pw, Math.ceil(fr.pw - fr.M * fr.dpr)), Math.min(fr.ph, Math.ceil(fr.ph - fr.M * fr.dpr))];
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
  // ---- rectangles [x0, y0, x1, y1], device px --------------------------------------
  const area = (r) => (r[2] - r[0]) * (r[3] - r[1]);
  const meet = (a, b) => {
    const r = [Math.max(a[0], b[0]), Math.max(a[1], b[1]), Math.min(a[2], b[2]), Math.min(a[3], b[3])];
    return r[2] > r[0] && r[3] > r[1] ? r : null;
  };
  /** `a` without `b` (which lies inside it): up to four bands. */
  function minus(a, b) {
    const out = [];
    if (b[1] > a[1]) out.push([a[0], a[1], a[2], b[1]]);
    if (b[3] < a[3]) out.push([a[0], b[3], a[2], a[3]]);
    if (b[0] > a[0]) out.push([a[0], b[1], b[0], b[3]]);
    if (b[2] < a[2]) out.push([b[2], b[1], a[2], b[3]]);
    return out;
  }
  /** The rectangles of frame `fr` a drawing of frame `old` copied at offset `sh` leaves uncovered. */
  const strips = (fr, sh, old) => {
    const all = [0, 0, fr.pw, fr.ph];
    const kept = meet(all, [sh[0], sh[1], sh[0] + old.pw, sh[1] + old.ph]);
    return kept ? minus(all, kept) : [all];
  };

  // ---- layers and parts ----------------------------------------------------------------
  // Each holds, besides its canvas, the version and frame it is drawn for
  // and `pend`: the rectangles of that frame still to draw (the margin, or
  // anything not needed on screen yet), drawn in idle time.
  let spare = null;
  /** Size `ly` for `fr`, keeping its pixels at offset `sh` when given. */
  function sizeLayer(ly, fr, sh) {
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
      if (ly.inPage) {
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
    if (ly.inPage) ly.cv.style.left = ly.cv.style.top = -fr.M + 'px';
  }
  /**
   * Make `ly` stand for frame `fr` at version `ver`, drawing nothing: when
   * only the view moved by whole pixels at the same version, its pixels are
   * copied along (and so are the rectangles it still owed) and the
   * uncovered strips are owed; else the whole of it is. A layer in the page
   * fills what it now owes with the background (the margin a gesture may
   * show before it is drawn) unless only the version changed (a filter: the
   * old drawing there is the better stand-in).
   */
  function bring(ly, fr, ver) {
    if (ly.ver === ver && ly.fk === fr.key) return false;
    const old = ly.fr;
    const sh = ly.ver === ver && old ? shiftOf(old, fr) : null;
    const sameFrame = old && ly.fk === fr.key;
    sizeLayer(ly, fr, sh);
    let pend;
    // (What a copied pan uncovers, filled with the background below; the
    // rectangles it owed and kept are drawn copies, not filled.)
    let fill;
    if (sh) {
      const all = [0, 0, fr.pw, fr.ph];
      fill = strips(fr, sh, old);
      pend = fill.slice();
      for (const r of ly.pend) {
        const m = meet(all, [r[0] + sh[0], r[1] + sh[1], r[2] + sh[0], r[3] + sh[1]]);
        if (m) pend.push(m);
      }
      stats.shifted++;
    } else {
      pend = [[0, 0, fr.pw, fr.ph]];
      fill = pend;
      stats.full++;
    }
    if (ly.inPage && !sameFrame) {
      const ctx = ctxOf(ly);
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.globalAlpha = 1;
      ctx.fillStyle = S.T.bg;
      for (const r of fill) ctx.fillRect(r[0], r[1], r[2] - r[0], r[3] - r[1]);
    }
    ly.ver = ver;
    ly.fr = fr;
    ly.fk = fr.key;
    ly.pend = pend.length > PEND_MAX ? [sc.boundsOf(pend)] : pend;
    ly.ms = 0;
    return true;
  }
  /**
   * Draw what `ly` owes inside `region` (all of it when null; a rectangle,
   * or a list of them) now, in one pass through `ly.draw`. An owed
   * rectangle that more than one region of a list cuts is drawn there and
   * stays owed whole (cutting it into the bands around many discs would
   * only be merged back into its bounds).
   */
  function settle(ly, region) {
    if (!ly.pend.length) return false;
    const t0 = performance.now();
    const rest = [];
    const now = [];
    const many = region && Array.isArray(region[0]);
    for (const r of ly.pend) {
      if (many) {
        const ms = [];
        for (const q of region) {
          const m = meet(r, q);
          if (m) ms.push(m);
        }
        if (ms.length === 1 && ms[0][0] === r[0] && ms[0][1] === r[1] && ms[0][2] === r[2] && ms[0][3] === r[3]) now.push(r);
        else {
          rest.push(r);
          if (ms.length === 1) {
            rest.pop();
            rest.push(...minus(r, ms[0]));
          }
          now.push(...ms);
        }
        continue;
      }
      const m = region ? meet(r, region) : r;
      if (!m) {
        rest.push(r);
        continue;
      }
      now.push(m);
      if (m !== r) rest.push(...minus(r, m));
    }
    ly.pend = rest;
    if (!now.length) return false;
    ly.draw(now);
    ly.ms += performance.now() - t0;
    return true;
  }
  const verOf = { below: () => `${tokVer}`, over: () => `${tokVer}`, faint: () => `${tokVer}` };
  const verLayer = () => `${tokVer}|${edgeVer}|${stubVer}|${tagVer}`;
  /** Whether part `ly` holds frame `fr` (it may still owe rectangles: `settle` them first). */
  const partAt = (ly, fr) => ly.ver === verOf[ly.name]() && ly.fk === fr.key;
  // The scratch canvas a faded group is drawn whole into, at least the
  // rectangle's size; let go of whenever idle work is done and nothing is
  // focused.
  let scratch = null;
  function scratchFor(w, hh) {
    scratch = scratch || document.createElement('canvas');
    if (scratch.width < w || scratch.height < hh) {
      scratch.width = Math.max(scratch.width, w);
      scratch.height = Math.max(scratch.height, hh);
    }
    return scratch;
  }
  const dropScratch = (over = 0) => {
    if (scratch && scratch.width * scratch.height > over) scratch.width = scratch.height = 0;
  };
  /** Rows of `r` at most `px` device pixels each. */
  function bands(r, px) {
    const rows = Math.max(8, Math.floor(px / (r[2] - r[0])));
    const out = [];
    for (let y = r[1]; y < r[3]; y += rows) out.push([r[0], y, r[2], Math.min(r[3], y + rows)]);
    return out;
  }
  B.draw = (rs) => sc.drawPart(ctxOf(B), S, B.fr, 'below', rs);
  // (Small rectangles, a memory's disc each, draw only what reaches them.)
  const few = (rs, fr) => rs.reduce((a, r) => a + area(r), 0) < FEW_SHARE * fr.pw * fr.ph;
  O.draw = (rs) => sc.drawPart(ctxOf(O), S, O.fr, 'over', rs, few(rs, O.fr));
  F.draw = (rs) => {
    const b = sc.boundsOf(rs);
    sc.drawPart(ctxOf(F), S, F.fr, 'faint', rs, few(rs, F.fr), scratchFor(b[2] - b[0], b[3] - b[1]));
  };
  L.base.draw = (rs) => {
    const fr = L.base.fr;
    if (partAt(B, fr) && partAt(O, fr)) {
      // (The rectangles themselves, not their bounds: a few hundred
      // scattered discs span the box, and a part still owing all of it
      // would be drawn whole here.)
      settle(B, rs);
      settle(O, rs);
      sc.composeUnfaded(ctxOf(L.base), S, fr, rs, { below: B.cv, over: O.cv });
      stats.composed++;
    } else sc.drawUnfaded(ctxOf(L.base), S, fr, rs);
  };
  /** The faded picture of rectangles `rs` of `fr` into `ctx` at offset (dx, dy). */
  function faded(ctx, fr, rs, skip, dx, dy) {
    const useB = partAt(B, fr);
    // (The faint part holds every label: not for a picture that leaves some out.)
    const useF = !(skip && skip.size) && partAt(F, fr);
    const b = sc.boundsOf(rs);
    if (useB) settle(B, rs);
    if (useF) settle(F, rs);
    sc.drawFaded(ctx, S, fr, rs, { scratch: scratchFor(b[2] - b[0], b[3] - b[1]), below: useB ? B.cv : null, faint: useF ? F.cv : null, skip, dx, dy });
  }
  /** The device boxes, inside `r`, of the base labels of the memories in `skip` (frame `fr`). */
  function labelHoles(fr, r, skip) {
    const out = [];
    for (const it of sc.baseLabels(S, fr)) {
      if (!skip.has(it.i)) continue;
      const m = meet(toDevice(sc.labelBox(S, it.i, it.x, it.y, it.anchor, 600), fr), r);
      if (m) out.push(m);
    }
    return out;
  }
  L.dim.draw = (rs) => faded(ctxOf(L.dim), L.dim.fr, rs, null, 0, 0);
  // A 1 × 1 canvas to draw a finished band into: that makes the browser
  // raster it now, in idle time, rather than at the paint that first shows
  // it (a canvas records its drawing until something needs pixels).
  let sink = null;
  const rasterNow = (cv) => {
    sink = sink || document.createElement('canvas');
    sink.width = sink.height = 1;
    sink.getContext('2d').drawImage(cv, 0, 0, 1, 1, 0, 0, 1, 1);
  };
  /** Memories whose look changed (`setHub`): their discs are owed by every layer and part that draws memories. */
  let nodesChanged = [];
  let nodeFrame = 0;
  function owe(list) {
    stats.owed += list.length;
    for (const ly of [L.base, L.dim, O, F]) {
      if (!ly.fr) continue;
      for (const i of list) {
        const nd = nodes[i];
        const q = sc.nodeReach(nd);
        const d = toDevice([nd.x - q, nd.y - q, nd.x + q, nd.y + q], ly.fr);
        if (d[2] > d[0] && d[3] > d[1]) ly.pend.push(d);
      }
      if (ly.pend.length > PEND_MAX) ly.pend = [sc.boundsOf(ly.pend)];
    }
  }

  // ---- idle work -------------------------------------------------------------------------
  // What is not on screen yet, a band per step, as many steps as the idle
  // period has room for (each step's cost estimated from the last ones of
  // its kind, so a slow machine takes smaller bands and no step is a long
  // task): the shown layer's margin, then the parts (a filter composes from
  // them), then the hidden one of base / dim (a focus, or its end, finds it
  // ready). Each band is forced to raster then (`rasterNow`).
  /** ms per device pixel of each kind of step, measured. */
  const cost = { base: 2e-5, dim: 4e-5, below: 2e-5, over: 2e-5, faint: 3e-5 };
  const costSeen = {};
  let pumping = false;
  // Set when an idle callback ran only at its timeout: the rest of the owed
  // work goes on in background tasks until none is left.
  let starved = false;
  function schedule() {
    if (pumping || !alive()) return;
    pumping = true;
    if (starved) bgCall(pump);
    else if (typeof requestIdleCallback === 'function') requestIdleCallback(pump, { timeout: 100 });
    else bgCall(pump);
  }
  /** The next owed work for frame `fr`: a layer or part with rectangles left, brought to the frame first. */
  function nextWork(fr) {
    const focus = !!plan;
    const shown = focus ? L.dim : L.base;
    // The parts first: the shown margin then composes from them, and a
    // filter toggle (or a hover's faded copy) that lands meanwhile finds
    // them current.
    for (const ly of [B, O, F]) {
      bring(ly, fr, verOf[ly.name]());
      if (ly.pend.length) return ly;
    }
    if (shown.pend.length) return shown;
    const hidden = focus ? L.base : L.dim;
    bring(hidden, fr, verLayer());
    if (hidden.pend.length) return hidden;
    return null;
  }
  function pump(deadline) {
    pumping = false;
    if (!view || !alive() || flat) return;
    const fr = frame();
    const t0 = performance.now();
    starved = !deadline || deadline.didTimeout;
    const room = starved ? STEP_MS : Math.min(ROOM_MS, deadline.timeRemaining());
    for (let n = 0; ; n++) {
      const ly = nextWork(fr);
      if (!ly) {
        starved = false;
        dropScratch();
        return;
      }
      const left = room - (performance.now() - t0);
      // (A step can cost twice its estimate: none starts without room for one.)
      if (n > 0 && left < STEP_MS) break;
      const k = ly.name;
      // A kind's first step (its canvas's first drawing: the backing store
      // is allocated then, ~30 ms at a 4x slower CPU whatever the band) is
      // the smallest band, alone in its callback, and teaches no cost.
      const first = !costSeen[k];
      if (first && n > 0) break;
      const px = first ? MIN_BAND_PX : Math.max(MIN_BAND_PX, Math.min(MAX_BAND_PX, Math.min(left - 2, STEP_MS) / (1.5 * cost[k])));
      const r = ly.pend[0];
      const band = area(r) <= px ? r : bands(r, px)[0];
      if (band === r) ly.pend.shift();
      else ly.pend[0] = [r[0], band[3], r[2], r[3]];
      const t1 = performance.now();
      ly.draw([band]);
      rasterNow(ly.cv);
      const ms = performance.now() - t1;
      ly.ms += ms;
      costSeen[k] = true;
      if (first) {
        if (k === 'base' || k === 'dim') stats.ms[k] = ly.ms;
        break;
      }
      cost[k] = 0.5 * cost[k] + (0.5 * ms) / area(band);
      if (k === 'base' || k === 'dim') stats.ms[k] = ly.ms;
    }
    dropScratch();
    schedule();
  }
  /** Whether nothing is owed (for the browser checks). */
  function idleNow() {
    if (!view || pumping || queued) return false;
    const fr = frame();
    const cur = (ly, ver) => ly.ver === ver && ly.fk === fr.key && !ly.pend.length;
    return cur(L.base, verLayer()) && cur(L.dim, verLayer()) && cur(B, verOf.below()) && cur(O, verOf.over()) && cur(F, verOf.faint());
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
    // The canvas grows to the largest patch at this ratio (a resize
    // reallocates it and makes the next font set re-resolve its style), and
    // shrinks back once a patch needs under a quarter of it.
    if (P.w < w || P.h < hh || P.dpr !== fr.dpr || P.w * P.h > 4 * w * hh) {
      const keep = P.dpr === fr.dpr && P.w * P.h <= 4 * w * hh;
      P.cv.width = P.w = Math.max(keep ? P.w : 0, w);
      P.cv.height = P.h = Math.max(keep ? P.h : 0, hh);
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
    // What is under it: the shown layer, whatever it owes there drawn
    // first; with a focus, the faded copy without the lit memories' faded
    // labels.
    // (With a focus: the faded copy, current there, and the lit memories'
    // label boxes drawn again without their faded labels.)
    if (plan) {
      settle(L.dim, r);
      ctx.drawImage(L.dim.cv, -r[0], -r[1]);
      const skip = new Set(plan.on);
      const holes = labelHoles(fr, r, skip);
      if (holes.length) faded(ctx, fr, holes, skip, r[0], r[1]);
    } else {
      settle(L.base, r);
      ctx.drawImage(L.base.cv, -r[0], -r[1]);
    }
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
  /** Mid-gesture: the opaque layer the patch was drawn into, and where (`gesture`). */
  let flat = false;
  let flatRect = null;
  function invalidate() {
    dirty = true;
    if (queued) return;
    queued = true;
    queueMicrotask(flush);
  }
  // Set by the first drawing, never cleared.
  let drawnOnce = false;
  function flush() {
    queued = false;
    if (!view || !alive() || flat) return;
    const fr = frame();
    const focus = !!plan;
    // Only the shown one of base / dim is brought to the frame now, and
    // only the box of it drawn; the rest follows in idle time.
    const shown = focus ? L.dim : L.base;
    const t0 = performance.now();
    bring(shown, fr, verLayer());
    if (settle(shown, boxRect(fr))) {
      dirty = true;
      stats.ms[shown.name] = performance.now() - t0;
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
      }
    }
    // With a focus the opaque faded copy covers the base: only it and the
    // patch are shown (every visible canvas is a compositor layer).
    if (L.dim.cv.hidden === focus) L.dim.cv.hidden = !focus;
    if (L.base.cv.hidden !== focus) L.base.cv.hidden = focus;
    if (dirty) {
      dirty = false;
      drawPatch(fr);
    }
    // (A box-sized scratch from a faded drawing is let go of at once.)
    dropScratch(SCRATCH_KEEP);
    if (warm && !idleNow()) schedule();
  }

  // ---- light / dark, device pixel ratio ----------------------------------------------
  // The tokens are read again only when what they depend on changed: the
  // app sets `data-graph-style` on every route, to the same value mostly.
  const mq = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
  const themeKey = () => {
    const de = document.documentElement;
    return `${de.getAttribute('data-theme')}|${de.getAttribute('data-graph-style')}|${mq ? mq.matches : ''}`;
  };
  let themeNow = themeKey();
  const retheme = () => {
    if (!alive()) return;
    const k = themeKey();
    if (k === themeNow) return;
    themeNow = k;
    readTokens();
    invalidate();
  };
  const mo = new MutationObserver(retheme);
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-graph-style'] });
  if (mq) mq.addEventListener('change', retheme);
  // A move to a screen of another ratio changes no CSS size, so no resize
  // observer fires: a resolution query does, re-armed at each new ratio
  // (and removed by `dispose`).
  let ratioQ = null;
  function onRatio() {
    ratioQ = null;
    if (!alive()) return;
    watchRatio();
    invalidate();
  }
  function watchRatio() {
    if (!window.matchMedia) return;
    ratioQ = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
    ratioQ.addEventListener('change', onRatio, { once: true });
  }
  watchRatio();
  /**
   * Let go of everything once the host left the page (the view calls it, and
   * so does the first callback that finds the host gone): the listeners on
   * the document and media queries, which would keep the host alive, and
   * every canvas's backing store (each layer and part up to `MAX_PIXELS`).
   */
  function dispose() {
    if (disposed) return;
    disposed = true;
    mo.disconnect();
    if (mq) mq.removeEventListener('change', retheme);
    if (ratioQ) ratioQ.removeEventListener('change', onRatio);
    ratioQ = null;
    if (nodeFrame) cancelAnimationFrame(nodeFrame);
    nodeFrame = 0;
    for (const ly of [L.base, L.dim, L.patch, B, O, F]) {
      ly.cv.width = ly.cv.height = 0;
      ly.w = ly.h = 0;
      ly.ver = ly.fr = ly.fk = null;
      ly.pend = [];
    }
    L.patch.rect = null;
    for (const cv of [scratch, spare, sink]) if (cv) cv.width = cv.height = 0;
    scratch = spare = sink = null;
    view = null;
    plan = null;
    built = [];
    want = new Uint8Array(0);
    hitEdge = null;
    tags = [];
    S.recs = [];
    S.stubs = [];
    S.tags = [];
  }

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
    backingBytes: () => [L.base, L.dim, L.patch, B, O, F].reduce((a, ly) => a + 4 * ly.cv.width * ly.cv.height, 0) + (scratch ? 4 * scratch.width * scratch.height : 0),
    relationsDrawn: () => S.recs.length,
    listBuilt: () => made.length,
    listDone: () => !!buttons,
    /** Whether every layer and part is drawn whole for the frame now (nothing left for idle time). */
    idle: idleNow,
    /** Mark every part and layer stale, so each one is drawn whole again. */
    // (The parts first, so the layers compose from them as a filter does.)
    redrawAll: () => {
      if (!view) return;
      const fr = frame();
      for (const ly of [B, O, F]) {
        ly.ver = 'stale';
        bring(ly, fr, verOf[ly.name]());
        settle(ly, null);
      }
      for (const ly of [L.base, L.dim]) ly.ver = 'stale';
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
      // Owed a frame later: a filter's frame shows its relations, the one
      // after it the looks it changed (a few hundred memories drawn again cost a
      // 2k-memory filter ~7 ms at ratio 2, which made its frame miss a vsync).
      nodesChanged.push(i);
      if (!nodeFrame)
        nodeFrame = requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            nodeFrame = 0;
            if (!alive()) return;
            owe(nodesChanged);
            nodesChanged = [];
            invalidate();
          }),
        );
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
      // The same relations shown again: nothing to draw, so the layers keep
      // their version and a pan is copied.
      if (b === built && w.length === want.length && w.every((x, k) => x === want[k])) return;
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
      const same = (a, c) => a.i === c.i && a.x1 === c.x1 && a.y1 === c.y1 && a.x2 === c.x2 && a.y2 === c.y2 && a.w === c.w && a.contra === c.contra;
      if (many === S.stubsMany && next.length === S.stubs.length && next.every((st, j) => same(st, S.stubs[j]))) return;
      S.stubs = next;
      S.stubsMany = many;
      stubVer++;
      invalidate();
    },
    setTags(next) {
      // The same tags again (a settled view in cut mode): only their actions
      // are taken, no element is rebuilt and nothing is drawn again.
      const same = (a, c) => a.tg === c.tg && a.name === c.name && a.count === c.count && a.contradictions === c.contradictions && a.aria === c.aria;
      if (next.length === tags.length && next.every((tag, j) => same(tag, tags[j]))) {
        tags = next;
        return;
      }
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
    dispose,
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
        flatRect = P.rect;
      } else if (flat) {
        // That layer now holds the patch there: owed again (copied along
        // with it by a pan, and drawn with the box at the settle).
        flat.pend.push(flatRect);
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
