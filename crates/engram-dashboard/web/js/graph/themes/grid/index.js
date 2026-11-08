// Grid: the graph's original look, kept as an optional style. Memories sit
// on a grid like stations on a transit map; each top-level scope is a
// coloured line through its memories, with a two-letter badge at its start;
// relations are routed in horizontal, vertical and diagonal segments.
//
// Only a style may use flavour words, and this one keeps its own: the
// header's light/dark button reads "Night map" / "Day map" on the graph
// view while Grid is picked, and the page title is "<project> network".

import { s } from '../../../dom.js';
import { plural } from '../../../format.js';
import { slot, variant, code, typeShape } from '../../../scopes.js';
import { DIRECTED } from '../../../relations.js';
import { pathD, along, offset } from '../../geometry.js';
import { buildLegend } from '../../legend.js';
import { chevron, terminal, warnSign, stubCap, stubSample, edgeSample as sample } from './glyphs.js';

export const id = 'grid';
export const name = 'Grid';

export const strings = {
  schemeToggle: (dark) => (dark ? 'Day map' : 'Night map'),
  title: (project) => `${project} network`,
  intro: () => ['Each memory is coloured by its top-level scope; shape follows type.', 'Size follows reads.'],
  // How this style's glyphs look, in the legend and the notes.
  legendProposedOther: 'Any other proposed relation: faded',
  legendProposedContradiction: 'Proposed contradiction (dashed, ⚠)',
  noteDangling: (n) => `${plural(n, 'relation')} to a deleted memory (end marked ⊗)`,
  noteUnknown: (n) => `${plural(n, 'relation')} into a store that cannot be checked here (drawn to “?”)`,
};

/** Laid out by the server (src/layout/grid.rs, named in graph/layouts.js); no options. */
export const layout = { name: 'grid', options: {} };

/** Each scope's line through its memories (under the relations), and its badge (over them). */
export function drawBackdrop(g, lay) {
  const below = s('g', { class: 'scope-paths', 'aria-hidden': 'true' });
  for (const l of lay.chains) {
    if (!l.key || l.pts.length < 2) continue;
    below.appendChild(s('path', { class: ['ln', 'stroke-' + slot(l.slot), 'lv-' + variant(l.slot)], d: pathD(l.pts) }));
  }
  const above = s('g', { class: 'lbadges', 'aria-hidden': 'true' });
  for (const l of lay.chains) {
    if (!l.key) continue;
    const [x, y] = l.pts[0];
    above.appendChild(
      s('g', { class: 'lb', transform: `translate(${(x - 24).toFixed(1)},${(y - 22).toFixed(1)})` }, s('rect', { class: 'fill-' + slot(l.slot), x: -12, y: -12, width: 24, height: 24, rx: 5 }), s('text', { class: 'lbt-' + slot(l.slot) }, code(l.key))),
    );
  }
  return { below, above };
}

/** A hollow shape by type, outlined in the scope's colour, with a red ring when challenged. */
export function drawNode(n, r) {
  return {
    cls: ['stroke-' + slot(n.color_slot)],
    parts: [n.challenged ? s('circle', { class: 'chal-ring', r: r + 3.5 }) : null, typeShape(n.type, r)],
  };
}

/**
 * A relation along its routed path: a halo, the stroke, ⊣ bars on a
 * contradiction (⚠ midway while proposed), a chevron at a directed
 * relation's target, and an end cap on a stub. `ends.rs` / `ends.rt` are
 * the gaps to leave at the source and target memory.
 */
export function drawEdge(e, pts, ends) {
  const d = pathD(pts);
  const parts = [s('path', { class: 'halo', d }), s('path', { class: 'cn', d })];
  if (e.kind === 'contradicts') {
    const a = offset(pts, ends.rs);
    parts.push(terminal(a.x, a.y, a.ang));
    if (!ends.stub) {
      const b = offset(pts, ends.rt, true);
      parts.push(terminal(b.x, b.y, b.ang));
    }
    if (e.status === 'proposed') {
      const m = along(pts, 0.5);
      parts.push(warnSign(m.x, m.y, 0.85));
    }
  } else if (DIRECTED[e.kind]) {
    const b = offset(pts, ends.rt + 2, true);
    parts.push(chevron(b.x, b.y, b.ang + Math.PI));
  }
  // A cap names a far end that is gone (⊗) or not checked (?); a live
  // memory in another store gets none.
  if (ends.stub && (e.state === 'dangling' || e.state === 'unknown')) {
    const end = pts[pts.length - 1];
    parts.push(stubCap(end[0], end[1], e.state));
  }
  return { cls: [], parts };
}

export const edgeSample = sample;

/** A legend memory drawn like the graph's (hollow, scope-coloured outline). */
function legendNode(kind, type) {
  return s(
    'svg',
    { width: 18, height: 18, viewBox: '-9 -9 18 18', 'aria-hidden': 'true', class: 'lg-node' },
    s('g', { class: ['node', 'stroke-0', kind] }, kind === 'chal' ? s('circle', { class: 'chal-ring', r: 7.5 }) : null, type ? typeShape(type, 4.6) : s('circle', { class: 'sh', r: 5 })),
  );
}

export function legend(g, t) {
  return buildLegend(g, t, {
    scopeSample: (l) => s('svg', { width: 34, height: 10, 'aria-hidden': 'true' }, s('path', { class: ['ln', 'stroke-' + slot(l.slot), 'lv-' + variant(l.slot)], d: 'M3,5 H31' })),
    nodeSample: legendNode,
    edgeSample: sample,
    stubSample,
  });
}
