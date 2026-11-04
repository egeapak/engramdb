// Memory detail: body, usage, relations, metadata, timeline.

import { h, s, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, stamp, plural, label, shortId } from '../format.js';
import { badge, slot, scopeLabel, kindIcon, TYPE_LABEL } from '../scopes.js';
import { smallMultiples } from '../chart.js';
import { renderMarkdown } from '../markdown.js';
import { secHead, pill, statusPill, alertBanner, errorBox } from './common.js';
import { reviewControls } from './review.js';
import { edgeSample, kindLabel } from '../graph/glyphs.js';

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

/** The API's status words, from the memory file's `Active` / `NeedsReview` / `Challenged`. */
function statusKey(status) {
  return { Challenged: 'challenged', NeedsReview: 'needs_review' }[status] || 'active';
}

/** One edge row: direction, drawn sample, kind, status, the other memory. */
function edgeItem(e, ctx) {
  const dir = e.symmetric ? '↔' : e.direction === 'out' ? 'Out →' : '← In';
  const other =
    e.state === 'unknown'
      ? h('span', { class: 'muted' }, `In store ${shortId(e.other_store)}, which cannot be checked here`)
      : e.state === 'dangling'
        ? h('span', { class: 'muted' }, 'Deleted memory')
        : e.other_store
          ? h('span', null, h('span', { class: 'muted' }, `In store ${shortId(e.other_store)} `), h('span', { class: 'mono' }, shortId(e.other_id)))
          : h('a', { href: ctx.href('detail', ctx.project.id, e.other_id) }, e.other_summary || e.other_id);
  return h(
    'li',
    null,
    h('span', { class: 'dir' }, dir),
    h(
      'div',
      null,
      h(
        'div',
        { class: 'rt' },
        edgeSample(e.kind, e.status, 40),
        kindLabel(e.kind),
        h('span', { class: ['ep', 'st-' + e.status] }, label(e.status)),
        e.state === 'invalidated' ? h('span', { class: 'ep invalidated' }, 'invalidated') : null,
      ),
      h('div', { class: 'other' }, other),
      h(
        'div',
        { class: 'org' },
        `${e.origin} · ${Number(e.confidence).toFixed(2)} · ${ago(e.created_at)}`,
        e.successor ? [' · replaced by ', h('a', { href: ctx.href('detail', ctx.project.id, e.successor) }, shortId(e.successor))] : null,
      ),
      e.note ? h('div', { class: 'org clamp' }, e.note) : null,
    ),
  );
}

function relations(d, ctx) {
  const edges = [...d.edges.outbound, ...d.edges.inbound];
  if (!edges.length) {
    return h('p', { class: 'muted' }, 'No relations. Agents link memories with the link tool; NLI and embedding similarity propose relations as memories are created.');
  }
  return [
    h('ul', { class: 'rels' }, edges.map((e) => edgeItem(e, ctx))),
    d.edges.hidden ? h('p', { class: 'faint' }, `${plural(d.edges.hidden, 'relation')} not shown.`) : null,
  ];
}

/** Proposed edges, each with its review controls. */
function review(d, ctx) {
  const m = d.memory;
  const proposed = [...d.edges.outbound, ...d.edges.inbound].filter((e) => e.status === 'proposed' && e.kind !== 'supersedes');
  if (!proposed.length) return null;
  return [
    secHead('Review', h('span', { class: 'faint num' }, plural(proposed.length, 'proposed relation'))),
    h(
      'ul',
      { class: ['att', 'plain', 'review-list'] },
      proposed.map((e) => {
        const out = e.direction === 'out';
        const edge = {
          src: out ? m.id : e.other_id,
          dst: out ? e.other_id : m.id,
          dst_store: out ? e.other_store : null,
          kind: e.kind,
          status: e.status,
          src_summary: out ? m.summary : e.other_summary,
          dst_summary: out ? e.other_summary : m.summary,
        };
        return h(
          'li',
          null,
          h('div', { class: 'ttl' }, edgeSample(e.kind, e.status, 40), ' ', kindLabel(e.kind), ': ', e.other_summary || shortId(e.other_id)),
          h('div', { class: 'sub' }, `Proposed by ${e.origin} · confidence ${Number(e.confidence).toFixed(2)} · ${ago(e.created_at)}`),
          reviewControls(edge, ctx, { viewer: m.id, onDone: () => setTimeout(() => ctx.refresh(), 600) }),
        );
      }),
    ),
  ];
}

function meta(m) {
  const rows = [
    ['Id', h('span', { class: 'mono' }, m.id)],
    ['Type', TYPE_LABEL[m.type] || m.type],
    ['Epistemic', m.epistemic],
    ['Status', label(statusKey(m.status))],
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
    { class: ['tl', 'l-' + slot(d.color_slot)] },
    d.timeline.map((e) =>
      h(
        'li',
        { class: e.kind === 'challenged' || e.kind === 'invalidated' ? 'warn' : null },
        h('div', { class: 'e' }, TIMELINE[e.kind] || label(e.kind)),
        h('div', { class: 'w' }, `${stamp(e.at)} · ${ago(e.at)}`),
        // Clamped: challenge evidence can run to paragraphs, and the
        // Challenges section has it in full.
        e.detail ? h('div', { class: 'ev clamp' }, e.detail) : null,
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
  const row = { status: statusKey(status), invalidated_at: m.invalidated_at };

  mount(
    main,
    h('a', { class: 'crumb', href: ctx.href('memories', p.id) }, '← All memories'),
    h('div', { class: 'sign' }, badge(d.top_scope, d.color_slot, true), h('div', null, h('div', { class: 'sc' }, scopeLabel(d.top_scope)), h('div', { class: 'sid mono' }, `${(m.logical || []).join(', ') || 'no logical scope'} · ${m.id}`))),
    h('h1', { class: 'd-title', id: 'view-title' }, title),
    h(
      'div',
      { class: 'd-pills' },
      h('span', { class: 'kind' }, kindIcon(m.type, d.color_slot), TYPE_LABEL[m.type] || m.type),
      pill(m.epistemic),
      statusPill(row),
      m.title && m.title !== m.summary ? h('span', { class: 'muted' }, m.summary) : null,
    ),
    challenged && newest
      ? alertBanner(
          'Challenged',
          `${plural(m.challenges.length, 'time')}, newest ${ago(newest.timestamp)}.`,
          h('span', { class: 'clamp' }, newest.evidence),
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
        secHead('Usage', h('span', { class: 'faint' }, 'exact, lifetime')),
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
        review(d, ctx),
        secHead('Relations', h('span', { class: 'faint num' }, fmt(d.edges.outbound.length + d.edges.inbound.length)), h('a', { class: 'btn sm', href: ctx.href('graph', p.id) }, 'Graph →')),
        relations(d, ctx),
        m.challenges && m.challenges.length
          ? [
              secHead('Challenges', h('span', { class: 'faint num' }, fmt(m.challenges.length))),
              h(
                'ul',
                { class: ['att', 'plain'] },
                m.challenges.map((c) =>
                  h(
                    'li',
                    null,
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
  smallMultiples(chartBox, {
    days: d.series.map((x) => x.day),
    rows: [
      { name: 'Retrieved', v: d.series.map((x) => x.returned), cls: 'bar-ret' },
      { name: 'Injected', v: d.series.map((x) => x.injected), cls: 'bar-inj' },
      { name: 'Fetched', v: d.series.map((x) => x.fetched), cls: 'bar-fet' },
    ],
    rowHeight: 40,
    aria: `Retrievals, injections and fetches per day for this memory, last 30 days.`,
  });
}
