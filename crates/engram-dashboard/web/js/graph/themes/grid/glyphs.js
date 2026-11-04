// The Grid style's relation (edge) drawing, used by its graph, its legend
// and, while Grid is the picked style, the samples on every other page (the
// relations table, the memory page, the overview), so a kind looks the same
// everywhere.
//
// Visual language:
//   contradicts   red segment; dashed while proposed,
//                 solid once confirmed; an inhibitory ⊣ bar at each end
//   supersedes    heavy ink line, arrow toward the old memory
//   supports      ink line, arrow toward what it supports
//   derived_from  dotted ink line, arrow toward the source it came from
//   related_to    thin grey line; duplicates thin grey dash-dot
//   proposed      dashed (any kind); rejected: faint dotted

import { s } from '../../../dom.js';
import { DIRECTED, edgeClass } from '../../../relations.js';

/** A chevron pointing along angle `ang` (radians) at (x, y). */
export function chevron(x, y, ang) {
  const deg = (ang * 180) / Math.PI;
  return s('path', { class: 'chev', d: 'M-5,-5 L1,0 L-5,5', transform: `translate(${x.toFixed(1)},${y.toFixed(1)}) rotate(${deg.toFixed(1)})` });
}

/** The ⊣ inhibitory bar across the line at (x, y), perpendicular to `ang`. */
export function terminal(x, y, ang) {
  const nx = -Math.sin(ang) * 6;
  const ny = Math.cos(ang) * 6;
  return s('path', { class: 'term', d: `M${(x - nx).toFixed(1)},${(y - ny).toFixed(1)} L${(x + nx).toFixed(1)},${(y + ny).toFixed(1)}` });
}

/** The open-contradiction warning sign, centred at (x, y). */
export function warnSign(x, y, size = 1) {
  return s(
    'g',
    { class: 'wsign', transform: `translate(${x.toFixed(1)},${y.toFixed(1)}) scale(${size})` },
    s('path', { class: 'tri', d: 'M0,-10 L11,8.5 L-11,8.5Z' }),
    s('rect', { class: 'mark', x: -1.3, y: -4, width: 2.6, height: 7, rx: 1 }),
    s('circle', { class: 'mark', cx: 0, cy: 5.6, r: 1.5 }),
  );
}

/** A small sample of a relation's drawing, for legends, tables and lists. */
export function edgeSample(kind, status, width = 44) {
  const y = 8;
  const x0 = 4;
  const x1 = width - 4;
  const parts = [s('path', { class: 'cn', d: `M${x0},${y} H${x1}` })];
  if (kind === 'contradicts') {
    parts.push(terminal(x0 + 1, y, 0), terminal(x1 - 1, y, 0));
  } else if (DIRECTED[kind]) {
    parts.push(chevron(x1 - 2, y, 0));
  }
  return s('svg', { class: edgeClass(kind, status), width, height: 16, viewBox: `0 0 ${width} 16`, 'aria-hidden': 'true' }, parts);
}

/** A stub's end cap: a crossed circle (dangling) or a question mark (unknown). */
export function stubCap(x, y, state) {
  if (state === 'unknown') {
    return s('g', { class: 'cap unknown', transform: `translate(${x.toFixed(1)},${y.toFixed(1)})` }, s('circle', { r: 7 }), s('text', { y: 0.5 }, '?'));
  }
  return s(
    'g',
    { class: 'cap closed', transform: `translate(${x.toFixed(1)},${y.toFixed(1)})` },
    s('circle', { r: 7 }),
    s('path', { d: 'M-3.5,-3.5 L3.5,3.5 M3.5,-3.5 L-3.5,3.5' }),
  );
}

/** A legend sample of a relation ending at a `dangling` or `unknown` far end. */
export function stubSample(state) {
  return s('svg', { width: 44, height: 16, viewBox: '0 0 44 16', 'aria-hidden': 'true', class: 'cx s-confirmed' }, s('path', { class: 'cn', d: 'M4,8 H30' }), stubCap(36, 8, state));
}
