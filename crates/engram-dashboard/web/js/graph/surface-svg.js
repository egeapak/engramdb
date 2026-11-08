// The SVG graph surface: every memory a `<g class="node">`, every relation a
// `<g class="cx">` with its click target, the style's backdrop around them,
// highlighting by classes that the style's CSS interprets. The surface for
// every SVG style (Grid), and for a canvas style whose canvas cannot start.
//
// A surface is what views/graph.js draws through; it owns the drawn
// elements and nothing else (data, filters, the view, selection, the
// keyboard and the panels stay in the view). The canvas surface
// (./canvas/host.js) has the same members:
//
//   el, pointerEl        what the view mounts in its `.g-pane`, and where
//                        it listens for pointer drags
//   composited           false: a drag pan sets the view box at once, and a
//                        settle waits for the view's 160 ms timer
//   nodeIndexOf(el)      the memory an element stands for, or -1
//   focusNode(i)         keyboard focus to memory i (roving: tabindex -1)
//   setHub(i, on)        memory i has 2+ drawn relations
//   setCur(i, on)        the ring of the panel row's neighbour
//   resetEdges()         the relations built so far are for another view
//   edge(k, e, pts, ends, cut)  build relation k (null: nothing to draw)
//   placeEdges(built, want)     show exactly the built relations in `want`
//   setStubs(stubs, many), setTags(tags)  an opened scope's far scopes
//   show(view, scale)    draw the view (here: the view box)
//   settled(scale)       a pan or zoom settled (the style's `zoomed`)
//   gesture(on)          a composited gesture starts / ends (canvas: one layer; nothing here)
//   highlight(plan)      light a memory / relation and its neighbours (null: none)
//   flush()              draw what is pending now (canvas; nothing here)
//
// `cb`: { hover(sel), pin(sel, focusCard), unpin(), quiet(), busy(), dragged() }
// from the view: `quiet()` while ] / [ moves focus without a hover,
// `busy()` mid-gesture (no hover), `dragged()` through a drag's click.

import { s, mount } from '../dom.js';
import { fmt } from '../format.js';
import { scopeName } from '../scopes.js';
import { edgeClass } from '../relations.js';
import { pathD } from './geometry.js';
import { reads, radius } from './radius.js';
import { nodeAria } from './a11y.js';

export function svgSurface({ g, lay, labels, theme, backdrop, cb }) {
  const P = (i) => [lay.pos[2 * i], lay.pos[2 * i + 1]];
  const nodeEls = [];
  // Per memory: its label element, `undefined` while not built yet (an
  // unplaced one), null for none.
  const labelEls = [];
  function makeLabel(i) {
    const n = g.nodes[i];
    const lp = lay.labels[i];
    const major = n.degree >= 2 || n.proposed > 0 || n.challenged;
    return s('text', { class: ['lbl', major && 'major', !lp.placed && 'unplaced'], x: lp.x, y: lp.y, 'text-anchor': lp.anchor }, labels[i]);
  }
  /** Memory `i`'s label element, built (in index order in its layer) on first need. */
  function labelEl(i) {
    if (labelEls[i] !== undefined) return labelEls[i];
    const el = makeLabel(i);
    let j = i + 1;
    while (j < labelEls.length && !labelEls[j]) j++;
    labelsLayer.insertBefore(el, j < labelEls.length ? labelEls[j] : null);
    labelEls[i] = el;
    return el;
  }
  const edgesLayer = s('g', { class: 'edges', 'aria-hidden': 'true' });
  // An opened scope's stubs (under the relations) and far-scope tags (over everything).
  const stubsLayer = s('g', { class: 'xs-stubs', 'aria-hidden': 'true' });
  const tagsLayer = s('g', { class: 'xs-tags' });
  let stubEls = g.nodes.map(() => []);
  // Labels sit under the memories, so a label never hides one.
  const labelsLayer = s('g', { class: 'labels', 'aria-hidden': 'true' });
  const nodesLayer = s('g', { class: 'nodes' });
  g.nodes.forEach((n, i) => {
    const [x, y] = P(i);
    const r = radius(n);
    const drawn = theme.drawNode(n, r);
    const el = s(
      'g',
      {
        class: ['node', ...(drawn.cls || []), n.degree >= 2 && 'hub', !reads(n) && 'never', n.challenged && 'chal', n.invalidated && 'inv'],
        transform: `translate(${x},${y})`,
        'data-i': i,
        tabindex: '-1',
        role: 'button',
        'aria-label': nodeAria(n),
      },
      // The hover / focus ring is the core's; the rest is the style's.
      s('circle', { class: 'ring', r: r + 6 }),
      drawn.parts,
    );
    nodeEls.push(el);
    nodesLayer.appendChild(el);
    // A placed label now; an unplaced one (no free spot: hidden, shown only
    // while its memory is highlighted, CSS .lbl.unplaced) the first time a
    // highlight needs it (`labelEl`), since most of a large graph's labels
    // have no spot and would only cost their creation.
    const lp = lay.labels[i];
    if (lp && lp.placed) {
      const lbl = makeLabel(i);
      labelEls.push(lbl);
      labelsLayer.appendChild(lbl);
    } else labelEls.push(lp ? undefined : null);
  });
  // Each memory's pointer, focus and key handling, delegated to the layer
  // (one listener per event instead of six per memory): the memory is the
  // `.node` the event came from, its index in `data-i`.
  const nodeOf = (ev) => {
    const el = ev.target instanceof Element ? ev.target.closest('.node') : null;
    return el && nodesLayer.contains(el) ? el : null;
  };
  // mouseenter / mouseleave: moves within one memory's own parts are not
  // an enter or a leave.
  const crossed = (el, other) => !(other instanceof Node && el.contains(other));
  nodesLayer.addEventListener('mouseover', (ev) => {
    // Mid-zoom the map moves under a still pointer: no hover until it settles.
    const el = cb.busy() ? null : nodeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) cb.hover({ node: +el.dataset.i });
  });
  nodesLayer.addEventListener('mouseout', (ev) => {
    const el = nodeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) cb.hover(null);
  });
  // A neighbour focused by ] / [ keeps the selection's highlight (stepRow).
  nodesLayer.addEventListener('focusin', (ev) => {
    const el = nodeOf(ev);
    if (el && ev.target === el && !cb.quiet()) cb.hover({ node: +el.dataset.i });
  });
  nodesLayer.addEventListener('focusout', (ev) => {
    const el = nodeOf(ev);
    if (el && ev.target === el) cb.hover(null);
  });
  nodesLayer.addEventListener('click', (ev) => {
    const el = nodeOf(ev);
    if (!el) return;
    ev.stopPropagation();
    if (!cb.dragged()) cb.pin({ node: +el.dataset.i }, false);
  });
  nodesLayer.addEventListener('keydown', (ev) => {
    const el = nodeOf(ev);
    if (el && (ev.key === 'Enter' || ev.key === ' ')) {
      ev.preventDefault();
      ev.stopPropagation();
      cb.pin({ node: +el.dataset.i }, true);
    }
  });

  const svg = s(
    'svg',
    { class: 'graph', role: 'group', 'aria-label': 'Memories' },
    s('rect', { class: 'bg', x: -1e5, y: -1e5, width: 2e5, height: 2e5, 'aria-hidden': 'true' }),
    backdrop.below || null,
    stubsLayer,
    edgesLayer,
    backdrop.above || null,
    labelsLayer,
    nodesLayer,
    tagsLayer,
  );
  svg.querySelector('.bg').addEventListener('click', () => {
    if (!cb.dragged()) cb.unpin();
  });

  // Pointer handling for every relation, on the layer (the relation is the
  // `[data-k]` group the event came from), as for the memories.
  const edgeOf = (ev) => {
    const el = ev.target instanceof Element ? ev.target.closest('[data-k]') : null;
    return el && edgesLayer.contains(el) ? el : null;
  };
  edgesLayer.addEventListener('mouseover', (ev) => {
    const el = cb.busy() ? null : edgeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) cb.hover({ edge: +el.dataset.k });
  });
  edgesLayer.addEventListener('mouseout', (ev) => {
    const el = edgeOf(ev);
    if (el && crossed(el, ev.relatedTarget)) cb.hover(null);
  });
  edgesLayer.addEventListener('click', (ev) => {
    const el = edgeOf(ev);
    if (!el || cb.dragged()) return;
    ev.stopPropagation();
    cb.pin({ edge: +el.dataset.k }, false);
  });

  let lit = [];
  let moved = [];
  let edgeEls = [];
  let zoomedOnce = false;

  return {
    el: svg,
    pointerEl: svg,
    composited: false,
    nodeIndexOf(el) {
      return el && el.classList && el.classList.contains('node') ? Number(el.getAttribute('data-i')) : -1;
    },
    focusNode(i) {
      nodeEls[i].focus({ preventScroll: true });
    },
    setHub(i, on) {
      nodeEls[i].classList.toggle('hub', on);
    },
    setCur(i, on) {
      if (nodeEls[i]) nodeEls[i].classList.toggle('cur', on);
    },
    resetEdges() {
      edgesLayer.replaceChildren();
    },
    edge(k, e, pts, ends, cut) {
      let drawn = theme.drawEdge(e, pts, ends);
      if (cut) drawn = { ...drawn, cls: [...(drawn.cls || []), 'cut'] };
      // The wide transparent hit path is the core's, drawn last (on top).
      const parts = [drawn.parts, s('path', { class: 'hit', d: pathD(pts) })];
      return s('g', { class: [...edgeClass(e.kind, e.status, e.state), ...(drawn.cls || [])], 'data-k': k }, parts);
    },
    placeEdges(built, want) {
      edgeEls = built;
      // Index order, back to front: each element goes before the one after it.
      let next = null;
      for (let k = built.length - 1; k >= 0; k--) {
        const el = built[k];
        if (!el) continue;
        if (want[k]) el.removeAttribute('hidden');
        else el.setAttribute('hidden', '');
        if (el.parentNode !== edgesLayer || el.nextSibling !== next) edgesLayer.insertBefore(el, next);
        next = el;
      }
    },
    setStubs(stubs, many) {
      stubEls = g.nodes.map(() => []);
      const lines = [];
      for (const st of stubs) {
        const el = s('line', {
          class: ['xs-stub', st.contra && 'contra'],
          x1: st.x1.toFixed(1),
          y1: st.y1.toFixed(1),
          x2: st.x2.toFixed(1),
          y2: st.y2.toFixed(1),
          'stroke-width': st.w.toFixed(2),
          'vector-effect': 'non-scaling-stroke',
        });
        stubEls[st.i].push(el);
        lines.push(el);
      }
      // Many stubs would veil the memories: past STUBS_FAINT they are drawn
      // fainter and come up with their memory's highlight.
      stubsLayer.classList.toggle('many', many);
      mount(stubsLayer, lines);
    },
    setTags(tags) {
      const tagEls = [];
      for (const tag of tags) {
        const { tg } = tag;
        const el = s(
          'g',
          { class: ['xs-tag', tag.contradictions && 'contra'], transform: `translate(${tg.x.toFixed(1)},${tg.y.toFixed(1)})`, tabindex: '0', role: 'link', 'aria-label': tag.aria },
          s('rect', { x: (-tg.w / 2).toFixed(1), y: (-tg.h / 2).toFixed(1), width: tg.w.toFixed(1), height: tg.h.toFixed(1), rx: (tg.h / 2).toFixed(1) }),
          s('text', { y: (tg.font * 0.36).toFixed(2), 'font-size': tg.font.toFixed(2), 'text-anchor': 'middle' }, `${scopeName(tag.name)} · ${fmt(tag.count)}`, tag.contradictions ? s('tspan', { class: 'w' }, `  ⚠ ${fmt(tag.contradictions)}`) : null),
        );
        el.addEventListener('click', (ev) => {
          ev.stopPropagation();
          if (!cb.dragged()) tag.go();
        });
        el.addEventListener('keydown', (ev) => {
          if (ev.key === 'Enter' || ev.key === ' ') {
            ev.preventDefault();
            ev.stopPropagation();
            tag.go();
          }
        });
        tagEls.push(el);
      }
      mount(tagsLayer, tagEls);
    },
    show(view, scale) {
      svg.setAttribute('viewBox', `${view.x.toFixed(1)} ${view.y.toFixed(1)} ${view.w.toFixed(1)} ${view.h.toFixed(1)}`);
      svg.classList.toggle('z-near', scale >= 0.8);
      svg.classList.toggle('z-mid', scale >= 0.5 && scale < 0.8);
      // A style may follow the zoom with one write on the <svg> (Plain: its
      // relations' width); never per relation. Even that one write restyles
      // every relation that reads it (measured: 58 → 30 fps zooming 534
      // relations), so it is made once the view settles, and once now for the
      // first view; mid-gesture relations scale with the map.
      if (theme.zoomed && !zoomedOnce) {
        zoomedOnce = true;
        theme.zoomed(svg, scale);
      }
    },
    settled(scale) {
      if (theme.zoomed) theme.zoomed(svg, scale);
    },
    /**
     * `plan`: { on: memories lit, nb: Set of the neighbours among them,
     * edges: relations lit, stubsOf: the memory whose stubs light (-1:
     * none), moves: [[i, x, y, anchor]] labels placed again among the lit
     * ones, nolbl: lit memories whose label found no spot }. Classes only:
     * `.has-focus` on the <svg> does the global fade (one class: never
     * every memory per hover), `.on` / `.nb` / `.nolbl` the rest.
     */
    highlight(plan) {
      for (const el of lit) el.classList.remove('on', 'nb', 'nolbl');
      lit = [];
      for (const [el, x, y, anchor] of moved) {
        el.setAttribute('x', x);
        el.setAttribute('y', y);
        el.setAttribute('text-anchor', anchor);
      }
      moved = [];
      svg.classList.toggle('has-focus', !!plan);
      if (!plan) return;
      const mark = (el) => {
        if (!el) return;
        el.classList.add('on');
        lit.push(el);
      };
      if (plan.stubsOf >= 0) for (const el of stubEls[plan.stubsOf]) mark(el);
      for (const k of plan.edges) mark(edgeEls[k]);
      for (const i of plan.on) {
        mark(nodeEls[i]);
        mark(labelEl(i));
        if (plan.nb.has(i)) {
          for (const el of [nodeEls[i], labelEl(i)]) {
            if (!el) continue;
            el.classList.add('nb');
            lit.push(el);
          }
        }
      }
      for (const i of plan.nolbl) {
        const el = labelEl(i);
        if (el) el.classList.add('nolbl');
      }
      for (const [i, x, y, anchor] of plan.moves) {
        const el = labelEl(i);
        if (!el) continue;
        moved.push([el, el.getAttribute('x'), el.getAttribute('y'), el.getAttribute('text-anchor')]);
        el.setAttribute('x', x);
        el.setAttribute('y', y);
        el.setAttribute('text-anchor', anchor);
      }
    },
    gesture() {},
    flush() {},
  };
}
