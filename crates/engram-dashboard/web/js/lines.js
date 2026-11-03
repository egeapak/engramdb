// Lines and stations: each top-level logical scope is a "line" with a
// colour; each memory is a station whose shape follows its type.

import { h, s } from './dom.js';

const SLOTS = 7;
let palette = new Map();

/**
 * Assign colour slots from a project's line counts (largest line first, as
 * the API orders them), so the same project always gets the same colours.
 */
export function setLines(lines) {
  palette = new Map();
  let i = 0;
  for (const l of lines || []) {
    if (!l.name) continue;
    palette.set(l.name, i % SLOTS);
    i += 1;
  }
}

/** The colour slot of a line: "0".."6", or "none". */
export function slot(line) {
  if (!line) return 'none';
  if (!palette.has(line)) palette.set(line, palette.size % SLOTS);
  return String(palette.get(line));
}

export function code(line) {
  if (!line) return '—';
  const letters = line.replace(/[^A-Za-z0-9]/g, '');
  return (letters.slice(0, 2) || line.slice(0, 2)).toUpperCase();
}

export function lineName(line) {
  return line || 'No scope';
}

export function badge(line, large) {
  const name = lineName(line);
  return h(
    'span',
    { class: ['badge', 'b-' + slot(line), large && 'lg'], title: `${name} line`, role: 'img', 'aria-label': `${name} line` },
    code(line),
  );
}

function shape(type, r) {
  switch (type) {
    case 'convention':
    case 'preference': {
      const q = r * 0.92;
      return [s('rect', { class: 'sh', x: -q, y: -q, width: 2 * q, height: 2 * q, rx: r * (type === 'preference' ? 0.6 : 0.28) })];
    }
    case 'hazard': {
      const k = r * 1.25;
      return [s('path', { class: 'sh', d: `M0,${-k} L${k},0 L0,${k} L${-k},0Z` })];
    }
    case 'debug': {
      const k = r * 1.2;
      return [s('path', { class: 'sh', d: `M0,${-k} L${k},${k * 0.8} L${-k},${k * 0.8}Z` })];
    }
    case 'context':
      return [s('circle', { class: 'sh', r }), s('circle', { class: 'dot', r: r * 0.36 })];
    case 'relationship':
      return [s('circle', { class: 'sh', r }), s('circle', { class: 'sh', r: r * 0.45 })];
    default:
      return [s('circle', { class: 'sh', r })];
  }
}

/** A 16px station glyph for a memory type, stroked in its line's colour. */
export function kindIcon(type, line) {
  return s(
    'svg',
    { width: 16, height: 16, viewBox: '-8 -8 16 16', 'aria-hidden': 'true', class: 'stroke-' + slot(line) },
    shape(type, 4.6),
  );
}

export function warnGlyph(size = 18) {
  return s(
    'svg',
    { width: size, height: size, viewBox: '-12 -12 24 24', 'aria-hidden': 'true', class: 'warn-glyph' },
    s('path', { class: 'tri', d: 'M0,-10.5 L11,8.5 L-11,8.5Z' }),
    s('rect', { class: 'mark', x: -1.4, y: -4.5, width: 2.8, height: 7.5, rx: 1 }),
    s('circle', { class: 'mark', cx: 0, cy: 5.6, r: 1.6 }),
  );
}

export const TYPE_LABEL = {
  context: 'Context',
  convention: 'Convention',
  decision: 'Decision',
  debug: 'Debug',
  hazard: 'Hazard',
  intent: 'Intent',
  preference: 'Preference',
  relationship: 'Relationship',
};
