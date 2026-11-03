// Lines and stations: each top-level logical scope is a "line" with a
// colour; each memory is a station whose shape follows its type.

import { h, s } from './dom.js';

const SLOTS = 7;

/**
 * The colour class of a line slot: "0".."6", or "none" for no line.
 *
 * Slots are decided by the server (`line_slot` on rows, `slot` on line
 * counts): one rank per line over the whole store, so a line has the same
 * colour on every page. Nothing here keeps state.
 */
export function slot(n) {
  return n === null || n === undefined ? 'none' : String(n % SLOTS);
}

/** 0 for the first seven lines, then 1 (ringed) and 2 (striped), so an eighth line never repeats a badge exactly. */
export function variant(n) {
  return n === null || n === undefined ? 0 : Math.min(2, Math.floor(n / SLOTS));
}

export function code(line) {
  if (!line) return '—';
  const letters = line.replace(/[^A-Za-z0-9]/g, '');
  return (letters.slice(0, 2) || line.slice(0, 2)).toUpperCase();
}

export function lineName(line) {
  return line || 'No scope';
}

/** A line's two-letter badge. `n` is its server-assigned slot. */
export function badge(line, n, large) {
  const name = lineName(line);
  const v = variant(n);
  return h(
    'span',
    { class: ['badge', 'b-' + slot(n), v && 'v-' + v, large && 'lg'], title: `${name} line`, role: 'img', 'aria-label': `${name} line` },
    code(line),
  );
}

/** A station glyph for a memory type, centred on 0,0 (stroke comes from the parent). */
export function stationShape(type, r) {
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

/** A 16px station glyph for a memory type, stroked in its line's colour (`n`: the line's slot). */
export function kindIcon(type, n) {
  return s(
    'svg',
    { width: 16, height: 16, viewBox: '-8 -8 16 16', 'aria-hidden': 'true', class: 'stroke-' + slot(n) },
    stationShape(type, 4.6),
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
