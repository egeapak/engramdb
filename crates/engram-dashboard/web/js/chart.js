// 30-day bar charts in SVG, built as DOM nodes.

import { h, s, mount } from './dom.js';
import { fmt, niceMax, dayLabel } from './format.js';

/**
 * @param box   container element (its width decides the chart's)
 * @param o     { days: ['YYYY-MM-DD'…], and either mode 'mirror' with a, b, classA, classB,
 *                nameA, nameB, or mode 'grouped' with series: [{ v, cls, name }],
 *                height, aria }
 */
export function barChart(box, o) {
  const W = Math.max(260, Math.floor(box.clientWidth || 600));
  const Ht = o.height || 210;
  const pl = 36, pr = 6, pt = 10, pb = 24;
  const iw = W - pl - pr, n = (o.a || o.series[0].v).length || 1, step = iw / n, bw = Math.max(2, step * 0.62);
  const nodes = [];
  const maxA = o.a ? niceMax(Math.max(1, ...o.a)) : 1;
  const gridLines = (max, y0, hh) =>
    [max, max / 2].filter((v) => v === Math.round(v)).forEach((v) => {
      const y = y0 - (v / max) * hh;
      nodes.push(s('line', { class: 'grid', x1: pl, x2: W - pr, y1: y, y2: y }));
      nodes.push(s('text', { class: 'axis', x: pl - 6, y: y + 3, 'text-anchor': 'end' }, fmt(v)));
    });
  if (o.mode !== 'grouped') {
    const tot = Ht - pt - pb, hA = tot * 0.72, y0 = pt + hA, hB = tot - hA - 6;
    const maxB = niceMax(Math.max(1, ...o.b));
    gridLines(maxA, y0, hA);
    nodes.push(s('text', { class: 'axis', x: pl - 6, y: y0 + 4 + hB, 'text-anchor': 'end' }, fmt(maxB)));
    for (let i = 0; i < n; i++) {
      const x = pl + i * step + (step - bw) / 2;
      const ha = (o.a[i] / maxA) * hA;
      if (ha > 0) nodes.push(s('rect', { class: o.classA, x: x.toFixed(1), y: (y0 - ha).toFixed(1), width: bw.toFixed(1), height: ha.toFixed(1) }));
      const hb = (o.b[i] / maxB) * hB;
      if (hb > 0) nodes.push(s('rect', { class: o.classB, x: x.toFixed(1), y: (y0 + 4).toFixed(1), width: bw.toFixed(1), height: hb.toFixed(1) }));
    }
    nodes.push(s('line', { class: 'base', x1: pl, x2: W - pr, y1: y0 + 2, y2: y0 + 2 }));
  } else {
    // Grouped bars, one slot per series inside each day: the signals stay
    // side by side, never stacked into a sum.
    const series = o.series;
    const hA = Ht - pt - pb, y0 = pt + hA;
    const maxS = niceMax(Math.max(1, ...series.flatMap((x) => x.v)));
    gridLines(maxS, y0, hA);
    const sw = bw / series.length;
    for (let i = 0; i < n; i++) {
      const x = pl + i * step + (step - bw) / 2;
      series.forEach((ser, j) => {
        const hv = (ser.v[i] / maxS) * hA;
        if (hv > 0) nodes.push(s('rect', { class: ser.cls, x: (x + j * sw).toFixed(1), y: (y0 - hv).toFixed(1), width: Math.max(1.5, sw - 0.5).toFixed(1), height: hv.toFixed(1) }));
      });
    }
    nodes.push(s('line', { class: 'base', x1: pl, x2: W - pr, y1: y0, y2: y0 }));
  }
  const ticks = [0, 7, 14, 21, n - 1].filter((k, i, arr) => k >= 0 && arr.indexOf(k) === i);
  for (const k of ticks) {
    const tx = pl + k * step + step / 2;
    const anchor = k === n - 1 ? 'end' : k === 0 ? 'start' : 'middle';
    nodes.push(s('text', { class: 'axis', x: tx.toFixed(1), y: Ht - 6, 'text-anchor': anchor }, k === n - 1 ? 'Today' : dayLabel(o.days[k])));
  }
  const all = o.series ? o.series.map((x) => x.v) : [o.a, o.b];
  if (all.every((v) => v.every((x) => !x))) {
    nodes.push(s('text', { class: 'empty', x: pl + iw / 2, y: pt + (Ht - pt - pb) / 2, 'text-anchor': 'middle' }, 'No activity recorded in the last 30 days'));
  }
  const hits = [];
  for (let i = 0; i < n; i++) {
    const r = s('rect', { class: 'hit', 'data-i': i, x: (pl + i * step).toFixed(1), y: pt, width: step.toFixed(1), height: Ht - pt - pb });
    hits.push(r);
    nodes.push(r);
  }
  const svg = s('svg', { width: W, height: Ht, viewBox: `0 0 ${W} ${Ht}`, role: 'img', 'aria-label': o.aria }, nodes);
  const tip = h('div', { class: 'tip', hidden: true });
  mount(box, svg, tip);
  hits.forEach((r, k) => {
    r.addEventListener('mouseenter', () => {
      const rows = o.series ? o.series.map((x) => [x.name, x.v[k]]) : [[o.nameA, o.a[k]], [o.nameB, o.b[k]]];
      mount(tip, h('b', null, k === n - 1 ? 'Today' : dayLabel(o.days[k])), rows.map(([nm, v]) => [h('br'), `${nm}: ${fmt(v)}`]));
      tip.hidden = false;
      const px = pl + k * step + step / 2, tw = tip.offsetWidth;
      // CSSOM writes are allowed under the CSP; only style *attributes* are not.
      tip.style.left = Math.min(Math.max(0, px - tw / 2), W - tw) + 'px';
    });
    r.addEventListener('mouseleave', () => {
      tip.hidden = true;
    });
  });
}
