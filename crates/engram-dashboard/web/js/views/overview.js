// Overview: big numbers, 30-day activity, needs-attention list, and the
// machine's other projects.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { fmt, ago, plural, shortId } from '../format.js';
import { badge, warnGlyph, kindIcon, TYPE_LABEL } from '../scopes.js';
import { smallMultiples } from '../chart.js';
import { projHead, secHead, alertBanner, schemaNote } from './common.js';
import { projectCards } from './projects.js';
import { reviewControls } from './review.js';
import { edgeSample, kindLabel } from '../graph/glyphs.js';

/** "+12%" / "−40%" against the previous window, or why there is none. */
function delta(now, before) {
  if (!before) return now ? 'new this week' : 'none either week';
  const d = Math.round(((now - before) / before) * 100);
  return `${d >= 0 ? '+' : '−'}${Math.abs(d)}% vs. week before`;
}

/** The read tile: three signals side by side, never one summed number. */
function readsStat(u) {
  const cell = (name, k) =>
    h('div', { class: 'trio-c' }, h('div', { class: 'tv' }, fmt(u.last_7_days[k])), h('div', { class: 'tn' }, name), h('div', { class: 'td' }, delta(u.last_7_days[k], u.previous_7_days[k])));
  return h(
    'div',
    { class: ['stat', 'wide'] },
    h('div', { class: 'eyebrow' }, 'Reads, last 7 days'),
    h('div', { class: 'trio' }, cell('Retrieved', 'returned'), cell('Injected', 'injected'), cell('Fetched', 'fetched')),
  );
}

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
  proposed_contradiction: 'Proposed contradictions',
  proposed_edge: 'Proposed relations',
  challenged: 'Challenged memories',
  needs_review: 'Needs review',
  stale: 'Stale: no reads in 30 days',
  never_read: 'Never read',
};

/** A proposed edge in the review queue: both ends, and the review controls behind a disclosure. */
function proposalItem(it, ctx) {
  const e = it.edge;
  const contra = e.kind === 'contradicts';
  const other = e.dst_store
    ? h('span', { class: 'muted' }, `a memory in store ${shortId(e.dst_store)}`)
    : h('a', { href: ctx.href('detail', ctx.project.id, e.dst) }, e.dst_summary || shortId(e.dst));
  const edge = { src: it.id, dst: e.dst, dst_store: e.dst_store, kind: e.kind, status: 'proposed', src_summary: it.summary, dst_summary: e.dst_summary };
  return h(
    'li',
    null,
    contra ? h('span', { class: 'ico' }, warnGlyph(22)) : h('span', { class: 'ico' }, edgeSample(e.kind, 'proposed', 22)),
    h('div', { class: 'ttl' }, h('a', { href: ctx.href('detail', ctx.project.id, it.id) }, it.summary)),
    h(
      'div',
      { class: 'sub' },
      `${kindLabel(e.kind).toLowerCase()} `,
      other,
      ` · ${e.origin} ${Number(e.confidence).toFixed(2)} · ${ago(it.at)}`,
    ),
    h('details', { class: 'review-disclosure' }, h('summary', null, 'Review'), reviewControls(edge, ctx, { onDone: () => setTimeout(() => ctx.refresh(), 600) })),
  );
}

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
            sub = [`${plural(it.count || 1, 'challenge')} · ${ago(it.at)}`];
            // Clamped to two lines; the memory's page has the full text.
            if (it.detail) sub.push(h('span', { class: 'clamp' }, `“${it.detail}”`));
          } else if (reason === 'needs_review') sub = [`Updated ${ago(it.at)}`];
          else if (reason === 'never_read') sub = [`Never read · created ${ago(it.at)}`];
          else sub = [`Last read ${ago(it.at)}`];
          if (it.edge) return proposalItem(it, ctx);
          sub.push(' · ', h('span', { class: 'mono' }, shortId(it.id)));
          return h(
            'li',
            null,
            reason === 'challenged' ? h('span', { class: 'ico' }, warnGlyph(22)) : badge(it.top_scope, it.color_slot),
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

  const days = st.series.map((d) => d.day);
  const col = (k) => st.series.map((d) => d[k]);
  const challenged = st.attention.filter((a) => a.reason === 'challenged');
  const scopes = st.by_scope.filter((l) => l.name).length;

  const meta = [h('span', { class: 'mono' }, p.path)];
  meta.push(`${p.last_write_at ? 'Last write ' + ago(p.last_write_at) : 'No writes yet'} · ${shortId(p.id)}`);

  const chartBox = h('div', { class: 'chart' });
  mount(
    main,
    projHead(p.kind === 'project' ? 'Project' : p.kind === 'group' ? 'Group store' : 'Global store', p.name, meta),
    schemaNote(st.schema, st.staleness),
    st.rebuilding ? h('div', { class: 'note warn' }, 'The index is empty but the store records memories: it is being rebuilt (a reindex is running). Reload in a moment.') : null,
    st.edges && st.edges.proposed_contradictions
      ? alertBanner(
          'Attention',
          `${plural(st.edges.proposed_contradictions, 'proposed contradiction')} awaiting review.`,
          null,
          h('a', { class: 'btn sm solid', href: ctx.href('graph', p.id) }, 'Graph'),
        )
      : null,
    challenged.length
      ? alertBanner(
          'Challenged',
          `${plural(challenged.length, 'memory', 'memories')} awaiting review.`,
          null,
          h('a', { class: 'btn sm solid', href: ctx.href('memories', p.id, 'status=challenged') }, 'Review'),
        )
      : null,
    h(
      'div',
      { class: 'stats' },
      stat(
        'Memories',
        fmt(st.live),
        `${scopes ? plural(scopes, 'top-level scope') : 'No scopes yet'} · ${plural(st.edges ? st.edges.total : 0, 'relation')}${st.edges && st.edges.proposed ? ` (${fmt(st.edges.proposed)} proposed)` : ''}${st.invalidated ? ` · ${fmt(st.invalidated)} invalidated` : ''}`,
      ),
      readsStat(st.usage),
      stat('Memories in use', fmt(st.usage.used_memories), `of ${fmt(st.live)} read at least once`),
      stat('Challenged', fmt(challenged.length), challenged.length ? 'Contested, awaiting review' : 'Nothing to review', challenged.length > 0),
    ),
    h(
      'div',
      { class: 'ov-grid' },
      h(
        'div',
        null,
        secHead('Activity, last 30 days', h('span', { class: 'faint' }, 'per day, each row on its own scale')),
        chartBox,
        h(
          'p',
          { class: 'faint' },
          `Lifetime: ${fmt(st.usage.returned)} retrieved, ${fmt(st.usage.injected)} injected, ${fmt(st.usage.fetched)} fetched. Writes count every change to a memory.`,
        ),
        secHead('By type'),
        st.by_type.length
          ? h(
              'ul',
              { class: 'att' },
              st.by_type.map((t) =>
                h('li', null, h('span', { class: 'ico kind' }, kindIcon(t.name, null)), h('div', { class: 'ttl' }, TYPE_LABEL[t.name] || t.name), h('div', { class: 'sub' }, plural(t.count, 'memory', 'memories'))),
              ),
            )
          : h('p', { class: 'muted' }, 'No memories yet. Agents add them with the create tool, or run engramdb add.'),
      ),
      h('div', null, secHead('Needs attention', h('span', { class: 'faint num' }, plural(st.attention.length, 'item'))), attention(st.attention, ctx)),
    ),
    secHead('Projects on this machine', h('a', { href: ctx.href('projects') }, 'All stores →')),
    projectCards(ctx.state.catalog.projects.filter((q) => q.kind === 'project' && !q.parent_id).slice(0, 8), ctx),
  );
  smallMultiples(chartBox, {
    days,
    rows: [
      { name: 'Retrieved', v: col('returned'), cls: 'bar-ret' },
      { name: 'Injected', v: col('injected'), cls: 'bar-inj' },
      { name: 'Fetched', v: col('fetched'), cls: 'bar-fet' },
      { name: 'Writes', v: col('writes'), cls: 'bar-write' },
    ],
    aria: `Retrievals, injections, fetches and writes per day for ${p.name}, last 30 days.`,
  });
}
