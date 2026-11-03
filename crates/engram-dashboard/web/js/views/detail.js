// Memory detail: body, usage, relations, metadata, timeline.

import { h, s, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, stamp, plural, label } from '../format.js';
import { badge, slot, lineName, kindIcon, TYPE_LABEL } from '../lines.js';
import { barChart } from '../chart.js';
import { renderMarkdown } from '../markdown.js';
import { secHead, pill, statusPill, disruption, errorBox } from './common.js';

const TIMELINE = {
  created: 'Created',
  updated: 'Last updated',
  verified: 'Verified',
  challenged: 'Challenged',
  invalidated: 'Validity window closed',
  expires: 'Expires',
  last_returned: 'Last retrieved by a query',
  last_injected: 'Last injected by a hook',
  last_fetched: 'Last fetched by id',
};

function relGlyph() {
  return s(
    'svg',
    { width: 40, height: 12, viewBox: '0 0 40 12', 'aria-hidden': 'true' },
    s('path', { class: 'rel-line', d: 'M2,6 H36' }),
    s('path', { class: 'rel-chev', d: 'M31,1.5 L37,6 L31,10.5' }),
  );
}

function relations(d, ctx) {
  const items = [];
  for (const l of d.supersedes) items.push({ dir: 'Out →', label: 'Supersedes', link: l });
  if (d.superseded_by) items.push({ dir: '← In', label: 'Superseded by', link: d.superseded_by });
  if (!items.length) {
    return h('p', { class: 'muted' }, 'No relations yet. Typed edges between memories (supports, contradicts, derived from) arrive with the route map.');
  }
  return h(
    'ul',
    { class: 'rels' },
    items.map((it) =>
      h(
        'li',
        null,
        h('span', { class: 'dir' }, it.dir),
        h(
          'div',
          null,
          h('div', { class: 'rt' }, relGlyph(), it.label),
          h(
            'div',
            { class: 'other' },
            it.link.found ? h('a', { href: ctx.href('detail', ctx.project.id, it.link.id) }, it.link.summary) : h('span', { class: 'muted' }, 'Not in this store (deleted, or in another store)'),
          ),
          h('div', { class: 'org mono' }, it.link.id),
        ),
      ),
    ),
  );
}

function meta(m) {
  const rows = [
    ['Id', h('span', { class: 'mono' }, m.id)],
    ['Kind', TYPE_LABEL[m.type] || m.type],
    ['Epistemic', m.epistemic],
    ['Status', label(m.status === 'NeedsReview' ? 'needs_review' : String(m.status).toLowerCase())],
    ['Visibility', String(m.visibility).toLowerCase()],
    ['Criticality', Number(m.criticality).toFixed(2)],
    ['Confidence', Number(m.confidence).toFixed(2)],
  ];
  if (m.logical && m.logical.length) rows.push(['Logical', h('span', { class: 'mono' }, m.logical.join(', '))]);
  if (m.physical && m.physical.length) rows.push(['Path', h('span', { class: 'mono' }, m.physical.join(', '))]);
  if (m.tags && m.tags.length) rows.push(['Tags', h('div', { class: 'tags' }, m.tags.map((t) => h('span', null, t)))]);
  rows.push(['Created', `${stamp(m.created_at)} (${ago(m.created_at)})`]);
  rows.push(['Updated', `${stamp(m.updated_at)} (${ago(m.updated_at)})`]);
  if (m.verified_at) rows.push(['Verified', stamp(m.verified_at)]);
  if (m.valid_from) rows.push(['Valid from', stamp(m.valid_from)]);
  if (m.invalidated_at) rows.push(['Invalidated', stamp(m.invalidated_at)]);
  if (m.expires_at) rows.push(['Expires', stamp(m.expires_at)]);
  const prov = m.provenance || {};
  const source = [prov.source, prov.agent_id, prov.model].filter(Boolean).join(' · ');
  if (source) rows.push(['Source', source]);
  if (m.origin_task) rows.push(['Task', h('span', { class: 'mono' }, m.origin_task)]);
  if (m.source_sessions && m.source_sessions.length) rows.push(['Sessions', h('span', { class: 'mono' }, m.source_sessions.join(', '))]);
  return h('dl', { class: 'meta' }, rows.flatMap(([k, v]) => [h('dt', null, k), h('dd', null, v)]));
}

function timeline(d) {
  return h(
    'ul',
    { class: ['tl', 'l-' + slot(d.line)] },
    d.timeline.map((e) =>
      h(
        'li',
        { class: e.kind === 'challenged' || e.kind === 'invalidated' ? 'warn' : null },
        h('div', { class: 'e' }, TIMELINE[e.kind] || label(e.kind)),
        h('div', { class: 'w' }, `${stamp(e.at)} · ${ago(e.at)}`),
        e.detail ? h('div', { class: 'ev' }, e.detail) : null,
      ),
    ),
  );
}

export async function renderDetail(main, ctx) {
  const p = ctx.project;
  mount(main, h('p', { class: 'loading' }, 'Loading memory…'));
  let d;
  try {
    d = await api.memory(p.id, ctx.route.mid);
  } catch (e) {
    if (ctx.isStale()) return;
    mount(main, h('a', { class: 'crumb', href: ctx.href('memories', p.id) }, '← All memories'), errorBox(e));
    return;
  }
  if (ctx.isStale()) return;
  const m = d.memory;
  const u = d.usage;
  const title = m.title || m.summary;
  const chartBox = h('div', { class: 'chart' });
  const status = String(m.status);
  const challenged = status === 'Challenged' || (m.challenges && m.challenges.length && status !== 'Active');
  const newest = (m.challenges || []).slice().sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp))[0];
  const row = { status: status === 'Challenged' ? 'challenged' : status === 'NeedsReview' ? 'needs_review' : 'active', invalidated_at: m.invalidated_at };

  mount(
    main,
    h('a', { class: 'crumb', href: ctx.href('memories', p.id) }, '← All memories'),
    h('div', { class: 'sign' }, badge(d.line, true), h('div', null, h('div', { class: 'sc' }, `${lineName(d.line)} line`), h('div', { class: 'sid mono' }, `${(m.logical || []).join(', ') || 'no logical scope'} · ${m.id}`))),
    h('h1', { class: 'd-title', id: 'view-title' }, title),
    h(
      'div',
      { class: 'd-pills' },
      h('span', { class: 'kind' }, kindIcon(m.type, d.line), TYPE_LABEL[m.type] || m.type),
      pill(m.epistemic),
      statusPill(row),
      m.title && m.title !== m.summary ? h('span', { class: 'muted' }, m.summary) : null,
    ),
    challenged && newest
      ? disruption(
          `challenged ${plural(m.challenges.length, 'time')}, newest ${ago(newest.timestamp)}.`,
          newest.evidence,
        )
      : null,
    h(
      'div',
      { class: 'd-grid' },
      h(
        'div',
        null,
        h('div', { class: 'd-body' }, renderMarkdown(m.content)),
        m.details ? h('details', { class: 'd-details' }, h('summary', null, 'Details'), h('div', { class: 'd-body' }, renderMarkdown(m.details))) : null,
        secHead('Usage', h('div', { class: 'legend-inline' }, h('span', null, h('i', { class: 'sw sw-' + slot(d.line) }), 'Retrieved'), h('span', null, h('i', { class: 'sw sw-ink' }), 'Injected'), h('span', null, h('i', { class: 'sw sw-write' }), 'Fetched'))),
        h(
          'div',
          { class: 'usage' },
          [
            [u.returned, 'retrieved'],
            [u.injected, 'injected'],
            [u.fetched, 'fetched'],
            [u.updated, 'updated'],
            [u.challenged, 'challenged'],
          ].map(([n, t]) => h('div', null, h('b', null, fmt(n)), h('span', null, t))),
        ),
        h('p', { class: 'faint' }, `Across ${plural(u.returned_sessions, 'session')} retrieved and ${plural(u.injected_sessions, 'session')} injected. Viewing it here counts as nothing.`),
        chartBox,
        secHead('Relations', h('span', { class: 'faint num' }, fmt(d.supersedes.length + (d.superseded_by ? 1 : 0)))),
        relations(d, ctx),
        m.challenges && m.challenges.length
          ? [
              secHead('Challenges', h('span', { class: 'faint num' }, fmt(m.challenges.length))),
              h(
                'ul',
                { class: 'att' },
                m.challenges.map((c) =>
                  h(
                    'li',
                    null,
                    h('span', { class: 'ico' }),
                    h('div', { class: 'ttl' }, c.evidence),
                    h('div', { class: 'sub' }, [stamp(c.timestamp), c.agent_id ? ` · ${c.agent_id}` : '', c.source_file ? ` · ${c.source_file}` : ''].join('')),
                  ),
                ),
              ),
            ]
          : null,
      ),
      h('div', null, secHead('Metadata'), meta(m), secHead('Timeline', h('span', { class: 'faint' }, 'newest first')), timeline(d)),
    ),
  );
  barChart(chartBox, {
    days: d.series.map((x) => x.day),
    mode: 'grouped',
    series: [
      { v: d.series.map((x) => x.returned), cls: 'fill-' + slot(d.line), name: 'Retrieved' },
      { v: d.series.map((x) => x.injected), cls: 'bar-ink', name: 'Injected' },
      { v: d.series.map((x) => x.fetched), cls: 'bar-write', name: 'Fetched' },
    ],
    height: 170,
    aria: `Retrievals, injections and fetches per day for ${m.id}, last 30 days.`,
  });
}
