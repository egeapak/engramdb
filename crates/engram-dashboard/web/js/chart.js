// 30-day small-multiple bar charts in SVG, built as DOM nodes.
//
// One short row per signal on a shared day axis, each with its own scale,
// its own maximum label and its own 0 baseline. Read signals are never
// stacked, summed or mirrored against each other: a retrieval, a hook
// injection and a fetch by id are different things, and a row each keeps
// them apart while the shared axis still lines the days up.

import { h, s, mount } from './dom.js';
import { fmt, niceMax, dayLabel } from './format.js';

const PL = 96; // left gutter: row name and scale
const PR = 4;
const GAP = 10;
const AXIS = 20;

function draw(box, o) {
  const W = Math.max(240, Math.floor(box.clientWidth || 600));
  const RH = o.rowHeight || 50;
  const rows = o.rows;
  const n = rows[0].v.length || 1;
  const H = rows.length * (RH + GAP) + AXIS;
  const iw = W - PL - PR;
  const step = iw / n;
  const bw = Math.max(1.5, Math.min(step * 0.66, 18));
  const nodes = [];
  rows.forEach((row, r) => {
    const top = r * (RH + GAP);
    const y0 = top + RH;
    const peak = Math.max(0, ...row.v);
    const max = niceMax(Math.max(1, peak));
    nodes.push(s('text', { class: 'row-name', x: 0, y: top + 11 }, row.name));
    // A row with nothing in it shows only its 0, not an invented scale.
    if (peak > 0) {
      nodes.push(s('line', { class: 'grid', x1: PL, x2: W - PR, y1: top + 0.5, y2: top + 0.5 }));
      nodes.push(s('text', { class: 'axis', x: PL - 6, y: top + 8, 'text-anchor': 'end' }, fmt(max)));
    }
    nodes.push(s('text', { class: 'axis', x: PL - 6, y: y0, 'text-anchor': 'end' }, '0'));
    for (let i = 0; i < n; i++) {
      const hv = (row.v[i] / max) * RH;
      if (hv <= 0) continue;
      const x = PL + i * step + (step - bw) / 2;
      nodes.push(s('rect', { class: row.cls, x: x.toFixed(1), y: (y0 - Math.max(hv, 1.5)).toFixed(1), width: bw.toFixed(1), height: Math.max(hv, 1.5).toFixed(1) }));
    }
    nodes.push(s('line', { class: 'base', x1: PL, x2: W - PR, y1: y0 + 0.5, y2: y0 + 0.5 }));
  });
  const ticks = [0, 7, 14, 21, n - 1].filter((k, i, arr) => k >= 0 && k < n && arr.indexOf(k) === i);
  for (const k of ticks) {
    const tx = PL + k * step + step / 2;
    const anchor = k === n - 1 ? 'end' : k === 0 ? 'start' : 'middle';
    nodes.push(s('text', { class: 'axis', x: tx.toFixed(1), y: H - 4, 'text-anchor': anchor }, k === n - 1 ? 'Today' : dayLabel(o.days[k])));
  }
  if (rows.every((row) => row.v.every((x) => !x))) {
    nodes.push(s('text', { class: 'empty', x: PL + iw / 2, y: (H - AXIS) / 2, 'text-anchor': 'middle' }, 'No activity in the last 30 days'));
  }
  const hits = [];
  for (let i = 0; i < n; i++) {
    const r = s('rect', { class: 'hit', x: (PL + i * step).toFixed(1), y: 0, width: step.toFixed(1), height: H - AXIS });
    hits.push(r);
    nodes.push(r);
  }
  // The SVG is for sight; the table below carries the same numbers for a
  // screen reader or a keyboard user, who cannot hover a bar.
  const svg = s('svg', { width: W, height: H, viewBox: `0 0 ${W} ${H}`, 'aria-hidden': 'true', focusable: 'false' }, nodes);
  const tip = h('div', { class: 'tip', hidden: true });
  mount(box, svg, tip, table(o));
  hits.forEach((r, k) => {
    r.addEventListener('mouseenter', () => {
      mount(tip, h('b', null, k === n - 1 ? 'Today' : dayLabel(o.days[k])), rows.map((row) => [h('br'), `${row.name}: ${fmt(row.v[k])}`]));
      tip.hidden = false;
      const px = PL + k * step + step / 2, tw = tip.offsetWidth;
      // CSSOM writes are allowed under the CSP; only style *attributes* are not.
      tip.style.left = Math.min(Math.max(0, px - tw / 2), W - tw) + 'px';
    });
    r.addEventListener('mouseleave', () => {
      tip.hidden = true;
    });
  });
}

function table(o) {
  // Hidden by a wrapper: a table sizes to its content and ignores the 1px
  // width, which would push the page sideways; the wrapper's overflow clips it.
  const head = h('tr', null, h('th', { scope: 'col' }, 'Day'), o.rows.map((r) => h('th', { scope: 'col' }, r.name)));
  const body = o.days.map((d, i) => h('tr', null, h('th', { scope: 'row' }, d), o.rows.map((r) => h('td', null, fmt(r.v[i])))));
  return h('div', { class: 'sr-only' }, h('table', null, h('caption', null, o.aria), h('thead', null, head), h('tbody', null, body)));
}

/**
 * @param box  container element; its width decides the chart's, and the
 *             chart redraws when that width changes
 * @param o    { days: ['YYYY-MM-DD'…], rows: [{ name, v: [n…], cls }],
 *               rowHeight, aria }
 */
export function smallMultiples(box, o) {
  draw(box, o);
  if (typeof ResizeObserver !== 'function') return;
  let width = box.clientWidth;
  const ro = new ResizeObserver(() => {
    if (!box.isConnected) {
      ro.disconnect();
      return;
    }
    const w = box.clientWidth;
    if (Math.abs(w - width) < 4) return;
    width = w;
    draw(box, o);
  });
  ro.observe(box);
}
