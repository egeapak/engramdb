// Plain: the neutral graph style, and the default. Memories are placed by
// the core's scope-aware force layout (scope-force, computed by the server): no grid, no
// scope chains, no badges; relations are straight.
//
// Memories are one neutral fill (shape follows type): colour belongs to the
// relations, whose kind it names. Scopes are told apart by their soft
// region and their name. The first seven top scopes (colour slots 0–6) get
// seven tints; past seven the tints repeat with a texture over them: slots
// 7–13 a diagonal hatch, 14–20 a dot grid, 21–27 a cross-hatch. Within the
// first 28 scopes two that share a tint never share a texture; past that
// textures repeat too and only the region's name tells them apart (every
// region is named).
//
// Relations are drawn by ./relations.js: tapered wedges (wide at the
// source) instead of arrowheads, kind by colour and shape; see its docs.

import { s } from '../../../dom.js';
import { plural } from '../../../format.js';
import { slot, typeShape } from '../../../scopes.js';
import { buildLegend } from '../../legend.js';
import { regionArcs, texture } from './regions.js';
import { canvas as canvasHooks } from './canvas.js';
import { drawRelation, fadeDefs, relationSample, stubSwatch, zoomFactor, redrawOnZoom as redrawAt } from './relations.js';

export const id = 'plain';
export const name = 'Plain';

export const strings = {
  intro: () => ['Memories sit in shaded, named regions by top-level scope; shape follows type.', 'Size follows reads. A tapered relation runs from its wide end (the source) to its narrow end.'],
  legendProposedContradiction: 'Proposed contradiction (dashed zigzag)',
  legendProposedOther: 'Any other proposed relation: dashed; a directed one has a chevron pointing to the target',
  legendRejected: 'Rejected: faint and dotted',
  noteDangling: (n) => `${plural(n, 'relation')} to a deleted memory (fading, ends in a bar)`,
  noteUnknown: (n) => `${plural(n, 'relation')} into a store that cannot be checked here (fading out)`,
};

/** The core's scope-aware force layout (scope-force, computed by the server: src/layout/scope_force.rs); no options. */
export const layout = { name: 'scope-force', options: {} };

/**
 * Plain draws on a canvas (graph/canvas/host.js) with the hooks of
 * ./canvas.js; the SVG hooks below stay its fallback (a browser with no 2D
 * canvas) and draw the legend and every other page's samples.
 */
export const renderer = 'canvas';
export const canvas = canvasHooks;

/** A region texture's pattern id: `v` the texture, `c` its tint. */
const patternId = (v, c) => `pl-pat-${v}-${c}`;

function regionPattern(v, c) {
  const mark =
    v === 2
      ? s('circle', { class: ['tex', 'fill-' + c], cx: 4.5, cy: 4.5, r: 1.5 })
      : s('path', { class: ['tex', 'stroke-' + c], d: v === 1 ? 'M0,0 V9' : 'M0,0 V9 M0,0 H9' });
  return s('pattern', { id: patternId(v, c), width: 9, height: 9, patternUnits: 'userSpaceOnUse', patternTransform: v === 2 ? null : 'rotate(45)' }, mark);
}

/**
 * Under the relations, each named top scope's soft region: one path per
 * scope, the union of a disc around each of its memories (`lay.region_r`;
 * one path, so overlapping discs never darken), plus its texture past the
 * seventh scope (module docs). Over the relations, one name per top scope
 * where the layout found it a free spot (`lay.scopes`). The stub fade's
 * mask rides along in the lower layer.
 */
export function drawBackdrop(g, lay) {
  const defs = fadeDefs();
  const patterns = new Set();
  const below = s('g', { class: 'scope-regions', 'aria-hidden': 'true' }, defs);
  for (const a of regionArcs(g, lay)) {
    const d = a.d;
    const c = slot(a.slot);
    below.appendChild(s('path', { class: ['region', 'fill-' + c], d }));
    const v = texture(a.slot);
    if (v) {
      if (!patterns.has(patternId(v, c))) {
        patterns.add(patternId(v, c));
        defs.appendChild(regionPattern(v, c));
      }
      below.appendChild(s('path', { class: 'region-tex', d, fill: `url(#${patternId(v, c)})` }));
    }
  }
  const above = s('g', { class: 'scope-names', 'aria-hidden': 'true' });
  for (const nm of lay.scopes || []) {
    if (!nm.placed) continue;
    // A name over memories gets a faint backing box, so it reads between them.
    if (nm.backed) {
      const w = nm.w || nm.key.length * nm.size * 0.62;
      above.appendChild(s('rect', { class: 'scope-name-bg', x: (nm.x - w / 2).toFixed(1), y: (nm.y - nm.size * 0.96).toFixed(1), width: w.toFixed(1), height: (nm.size * 1.2).toFixed(1), rx: (nm.size * 0.3).toFixed(1) }));
    }
    above.appendChild(s('text', { class: 'scope-name', x: nm.x.toFixed(1), y: nm.y.toFixed(1), 'font-size': nm.size.toFixed(1), 'text-anchor': 'middle' }, nm.key));
  }
  return { below, above };
}

/** One neutral fill for every memory (module docs); shape follows type. */
export function drawNode(n, r) {
  return {
    cls: ['pl'],
    parts: [n.challenged ? s('circle', { class: 'chal-ring', r: r + 3.5 }) : null, typeShape(n.type, r)],
  };
}

/** One `<path>` per relation (./relations.js); the core adds the hit path. */
export function drawEdge(e, pts, ends) {
  return { cls: [], parts: [drawRelation(e, pts, ends)] };
}

let zw = null;

/**
 * The core calls this for the first view and once each pan / zoom settles,
 * with the scale (screen px per layout unit): one custom property on the
 * `<svg>`, written only when it moved, rescales every relation's width
 * (./relations.js). Mid-gesture the widths follow the map.
 */
export function zoomed(svg, scale) {
  const z = zoomFactor(scale);
  if (zw !== null && Math.abs(z - zw) / zw < 0.02 && svg.style.getPropertyValue('--zw')) return;
  zw = z;
  svg.style.setProperty('--zw', z.toFixed(3));
}

/** After a zoom settles: redraw the relations when it moved far from the drawn one (./relations.js). */
export const redrawOnZoom = redrawAt;

export const edgeSample = relationSample;

function legendNode(kind, type) {
  return s(
    'svg',
    { width: 18, height: 18, viewBox: '-9 -9 18 18', 'aria-hidden': 'true', class: 'lg-node' },
    s('g', { class: ['node', 'pl', kind] }, kind === 'chal' ? s('circle', { class: 'chal-ring', r: 7.5 }) : null, type ? typeShape(type, 4.6) : s('circle', { class: 'sh', r: 5 })),
  );
}

/** A scope's region swatch: its tint, and its texture past the seventh scope (none for "No scope", which has no region). */
function scopeSwatch(l) {
  if (!l.name) return null;
  const c = slot(l.slot);
  const v = texture(l.slot);
  const hatch = 'M-6,2 L2,-6 M-6,8 L8,-6 M2,8 L8,2';
  const tex =
    v === 1
      ? s('path', { class: ['tex', 'stroke-' + c], d: hatch })
      : v === 2
        ? [-3, 3].flatMap((x) => [-3, 3].map((y) => s('circle', { class: ['tex', 'fill-' + c], cx: x, cy: y, r: 1.4 })))
        : v === 3
          ? s('path', { class: ['tex', 'stroke-' + c], d: hatch + ' M-6,-2 L2,6 M-6,-8 L8,6 M2,-8 L8,-2' })
          : null;
  return s('svg', { width: 14, height: 14, viewBox: '-7 -7 14 14', 'aria-hidden': 'true', class: 'lg-region' }, s('rect', { class: ['region', 'fill-' + c], x: -6, y: -6, width: 12, height: 12, rx: 3 }), tex);
}

export function legend(g, t) {
  return buildLegend(g, t, {
    scopeSample: scopeSwatch,
    nodeSample: legendNode,
    edgeSample: relationSample,
    stubSample: stubSwatch,
    direction: true,
  });
}
