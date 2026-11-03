// Overview: big numbers, 30-day activity, needs-attention list, and the
// machine's other projects.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, plural, shortId } from '../format.js';
import { badge, setLines, warnGlyph, kindIcon } from '../lines.js';
import { barChart } from '../chart.js';
import { projHead, secHead, disruption, schemaNote } from './common.js';
import { projectCards } from './projects.js';

function stat(eyebrow, value, sub, alert) {
  return h(
    'div',
    { class: ['stat', alert && 'alert'] },
    h('div', { class: 'eyebrow' }, eyebrow),
    h('div', { class: 'v' }, alert ? warnGlyph(40) : null, value),
    h('div', { class: 's' }, sub),
  );
}

const REASON = {
  challenged: 'Service disruptions',
  needs_review: 'Needs review',
  stale: 'Stale: no reads in 30 days',
  never_read: 'Never read',
};

function attention(items, ctx) {
  if (!items.length) return h('p', { class: 'muted' }, 'Nothing needs attention.');
  const groups = new Map();
  for (const it of items) {
    if (!groups.has(it.reason)) groups.set(it.reason, []);
    groups.get(it.reason).push(it);
  }
  const out = [];
  for (const [reason, list] of groups) {
    out.push(h('div', { class: 'eyebrow att-group' }, REASON[reason] || reason));
    out.push(
      h(
        'ul',
        { class: 'att' },
        list.map((it) => {
          let sub;
          if (reason === 'challenged') {
            sub = [
              `${plural(it.count || 1, 'challenge')} · ${ago(it.at)}`,
              it.detail ? ` · “${it.detail}”` : '',
            ];
          } else if (reason === 'needs_review') sub = [`Updated ${ago(it.at)}`];
          else if (reason === 'never_read') sub = [`Never read · created ${ago(it.at)}`];
          else sub = [`Last read ${ago(it.at)}`];
          sub.push(' · ', h('span', { class: 'mono' }, shortId(it.id)));
          return h(
            'li',
            null,
            reason === 'challenged' ? h('span', { class: 'ico' }, warnGlyph(22)) : badge(it.line),
            h('div', { class: 'ttl' }, h('a', { href: ctx.href('detail', ctx.project.id, it.id) }, it.summary)),
            h('div', { class: 'sub' }, sub),
          );
        }),
      ),
    );
  }
  return out;
}

export async function renderOverview(main, ctx) {
  const p = ctx.project;
  if (!p.available) {
    mount(main, projHead('Project', p.name, [h('span', { class: 'mono' }, p.path)]), h('div', { class: 'note warn' }, `Unavailable: ${p.unavailable_reason || 'no readable store'}.`));
    return;
  }
  mount(main, h('p', { class: 'loading' }, 'Loading overview…'));
  const st = await api.stats(p.id);
  if (ctx.isStale()) return;
  setLines(st.by_line);

  const days = st.series.map((d) => d.day);
  const reads = st.series.map((d) => d.returned + d.injected + d.fetched);
  const writes = st.series.map((d) => d.writes);
  const r7 = st.usage.reads_last_7_days, p7 = st.usage.reads_previous_7_days;
  const delta = p7 ? Math.round(((r7 - p7) / p7) * 100) : null;
  const challenged = st.attention.filter((a) => a.reason === 'challenged');
  const lines = st.by_line.filter((l) => l.name).length;

  const meta = [h('span', { class: 'mono' }, p.path)];
  meta.push(`${p.last_write_at ? 'Last write ' + ago(p.last_write_at) : 'No writes yet'} · ${shortId(p.id)}`);

  const chartBox = h('div', { class: 'chart' });
  mount(
    main,
    projHead(p.kind === 'project' ? 'Project' : p.kind === 'group' ? 'Group store' : 'Global store', p.name, meta),
    schemaNote(st.schema, st.staleness),
    challenged.length
      ? disruption(
          `${plural(challenged.length, 'memory is', 'memories are')} challenged and awaiting review.`,
          null,
          h('a', { class: 'btn sm solid', href: ctx.href('memories', p.id, 'status=challenged') }, 'Review'),
        )
      : null,
    h(
      'div',
      { class: 'stats' },
      stat('Memories', fmt(st.live), `${plural(lines, 'line')}${st.invalidated ? ` · ${fmt(st.invalidated)} invalidated` : ''}`),
      stat('Reads, last 7 days', fmt(r7), delta === null ? 'No reads in the previous 7 days' : `${delta >= 0 ? '+' : '−'}${Math.abs(delta)}% vs. the previous 7 days`),
      stat('Memories in use', fmt(st.usage.used_memories), `of ${fmt(st.live)} read at least once`),
      stat('Challenged', fmt(challenged.length), challenged.length ? 'Contested, awaiting review' : 'Nothing to review', challenged.length > 0),
    ),
    h(
      'div',
      { class: 'ov-grid' },
      h(
        'div',
        null,
        secHead(
          'Activity, last 30 days',
          h('div', { class: 'legend-inline' }, h('span', null, h('i', { class: 'sw sw-read' }), 'Reads'), h('span', null, h('i', { class: 'sw sw-write' }), 'Writes')),
        ),
        chartBox,
        h(
          'p',
          { class: 'faint' },
          `Reads are retrieved + injected + fetched, kept apart everywhere else. Lifetime: ${fmt(st.usage.returned)} retrieved, ${fmt(st.usage.injected)} injected, ${fmt(st.usage.fetched)} fetched.`,
        ),
        secHead('By kind'),
        h(
          'ul',
          { class: 'att' },
          st.by_type.map((t) =>
            h('li', null, h('span', { class: 'ico kind' }, kindIcon(t.name, null)), h('div', { class: 'ttl' }, `${t.name.charAt(0).toUpperCase()}${t.name.slice(1)}`), h('div', { class: 'sub' }, plural(t.count, 'memory', 'memories'))),
          ),
        ),
      ),
      h('div', null, secHead('Needs attention', h('span', { class: 'faint num' }, plural(st.attention.length, 'item'))), attention(st.attention, ctx)),
    ),
    secHead('Projects on this machine', h('a', { href: ctx.href('projects') }, 'All stores →')),
    projectCards(ctx.state.catalog.projects.filter((q) => q.kind === 'project' && !q.parent_id).slice(0, 8), ctx),
  );
  barChart(chartBox, {
    days,
    a: reads,
    b: writes,
    mode: 'mirror',
    classA: 'bar-read',
    classB: 'bar-write',
    nameA: 'Reads',
    nameB: 'Writes',
    height: 230,
    aria: `Reads and writes per day for ${p.name}, last 30 days. ${fmt(reads.reduce((a, b) => a + b, 0))} reads, ${fmt(writes.reduce((a, b) => a + b, 0))} writes.`,
  });
}
