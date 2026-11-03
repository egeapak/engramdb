// Route map: a placeholder until memory edges land (phase 4).

import { h, s, mount } from '../dom.js';
import { projHead } from './common.js';

export async function renderGraph(main, ctx) {
  const p = ctx.project;
  const st = (x, y) => s('circle', { class: 'st', cx: x, cy: y, r: 9 });
  mount(
    main,
    projHead('Route map', `${p.name} network`, ['Lines are logical scopes. Stations are memories.', 'Arrives with typed memory edges.']),
    h(
      'div',
      { class: 'soon-map' },
      s(
        'svg',
        { width: 520, height: 150, viewBox: '0 0 520 150', role: 'img', 'aria-label': 'Sketch of a route map with three lines' },
        s('path', { class: 'ln stroke-0', d: 'M20,40 H300 L360,100 H500' }),
        s('path', { class: 'ln stroke-1', d: 'M20,110 H200 L260,40' }),
        s('path', { class: 'ln stroke-2', d: 'M140,20 V130' }),
        st(80, 40), st(200, 40), st(300, 40), st(430, 100), st(80, 110), st(140, 75), st(200, 110),
      ),
      h('h2', null, 'The route map is coming'),
      h(
        'p',
        { class: 'muted' },
        'It needs typed edges between memories (supports, contradicts, supersedes, derived from), which the next release adds. Until then, supersedes links and challenges are on each memory’s page.',
      ),
      h('a', { class: 'btn', href: ctx.href('memories', p.id) }, 'Browse memories →'),
    ),
  );
}
