// Edge review: confirm or reject a proposed edge, with an in-page confirm
// step before anything is sent. The dashboard's only write; the server
// refuses it unless started with --allow-writes, and the buttons say so.
//
// A `contradicts` edge has three ways to be confirmed (the plan's
// "Contradiction direction"): both stand in conflict, or one supersedes the
// other (only the loser's validity window closes) — plus reject, which keeps
// the edge as rejected so no proposer raises it again. Settling a
// contradiction also resolves the NLI challenge it came with; the server
// says on which memories, and the result line repeats it.
//
// The server only reviews an edge that is still proposed (a repeat is a
// no-op, anything else a 409). The page also remembers, per edge, a review
// in flight or done, so controls rebuilt for the same edge (another view,
// a re-render) never offer a second send.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { shortId } from '../format.js';
import { kindLabel } from '../graph/glyphs.js';

/** Per edge: {state: 'pending' | 'done', text}. Lives as long as the page. */
const reviews = new Map();

const edgeKey = (pid, e) => [pid, e.src, e.kind, e.dst, e.dst_store || ''].join('|');

function short(text, n = 32) {
  const t = String(text || '').trim();
  return t.length > n ? t.slice(0, n - 1) + '…' : t || 'this memory';
}

/** A memory named in a choice: its summary plus a short id, so two
 * summaries with the same opening still differ. */
const named = (summary, id) => `“${short(summary)}” (${shortId(id).slice(0, 8)})`;

/** The choices for one edge, as {label, consequence, send}. */
function choices(edge, viewer, pid) {
  const body = { src: edge.src, kind: edge.kind, dst: edge.dst };
  if (edge.dst_store) body.dst_store = edge.dst_store;
  const srcName = named(edge.src_summary, edge.src);
  const dstName = named(edge.dst_summary, edge.dst);
  const settle = ' Any NLI challenge between the two is resolved.';
  if (edge.kind === 'contradicts') {
    const sup = (winner, loserName, label) => ({
      label,
      consequence: `${loserName} will be marked superseded: its validity window closes and it drops out of default retrieval. The contradiction becomes a supersedes edge.${settle}`,
      send: () => api.confirmEdge(pid, { ...body, verdict: 'supersedes', winner }),
    });
    const out = [
      {
        label: 'Both stand (conflict)',
        consequence: `The contradiction is confirmed. Both memories stay valid, and the edge records the conflict.${settle}`,
        send: () => api.confirmEdge(pid, { ...body, verdict: 'both_stand' }),
      },
    ];
    if (!edge.dst_store) {
      if (viewer && (viewer === edge.src || viewer === edge.dst)) {
        const other = viewer === edge.src ? edge.dst : edge.src;
        const otherName = viewer === edge.src ? dstName : srcName;
        out.push(sup(viewer, otherName, 'This one supersedes the other'));
        out.push(sup(other, 'This memory', 'The other supersedes this one'));
      } else {
        out.push(sup(edge.src, dstName, `${srcName} supersedes the other`));
        out.push(sup(edge.dst, srcName, `${dstName} supersedes the other`));
      }
    }
    out.push({
      label: 'Not a contradiction',
      tone: 'reject',
      consequence: `The edge is kept as rejected, so it is never proposed again.${settle}`,
      send: () => api.rejectEdge(pid, body),
    });
    return out;
  }
  return [
    {
      label: 'Confirm',
      consequence: `The ${kindLabel(edge.kind).toLowerCase()} edge becomes confirmed.`,
      send: () => api.confirmEdge(pid, body),
    },
    {
      label: 'Reject',
      tone: 'reject',
      consequence: 'The edge is kept as rejected, so it is never proposed again.',
      send: () => api.rejectEdge(pid, body),
    },
  ];
}

/** What the server did, in a sentence. */
function resultText(o, out, edge) {
  if (out && out.changed === false) return `Already done: ${o.label}. Nothing changed.`;
  let text = `Done: ${o.label}.`;
  const ids = (out && out.resolved_challenges) || [];
  if (ids.length) {
    const name = (id) => (id === edge.src ? named(edge.src_summary, id) : id === edge.dst ? named(edge.dst_summary, id) : shortId(id));
    text += ` Resolved the NLI challenge on ${ids.map(name).join(' and ')}.`;
  }
  return text;
}

/**
 * Review controls for `edge` ({src, dst, dst_store?, kind, status,
 * src_summary?, dst_summary?}). `viewer` is the memory whose page this is,
 * for "this one / the other" wording. `onDone(out, text)` runs after a
 * successful write, with the server's answer and the sentence shown.
 */
export function reviewControls(edge, ctx, { viewer, onDone } = {}) {
  if (edge.kind === 'supersedes') return null;
  const pid = ctx.project.id;
  const key = edgeKey(pid, edge);
  const writes = !!(ctx.state.session && ctx.state.session.allow_writes);
  const box = h('div', { class: 'review', role: 'group', 'aria-label': `Review: ${kindLabel(edge.kind)}` });
  const live = h('p', { class: 'review-status', role: 'status', 'aria-live': 'polite', tabindex: '-1' });
  const noteId = 'ro-' + Math.random().toString(36).slice(2);

  function showChoices(focusIndex) {
    const known = reviews.get(key);
    if (known) {
      live.textContent = known.text;
      mount(box, live);
      return;
    }
    const opts = choices(edge, viewer, pid);
    const buttons = opts.map((o, i) =>
      h(
        'button',
        {
          type: 'button',
          // Neutral: no choice is the default answer.
          class: ['btn', 'sm', o.tone === 'reject' ? 'ghost' : null],
          disabled: !writes,
          'aria-describedby': writes ? null : noteId,
          onclick: () => ask(o, i),
        },
        o.label,
      ),
    );
    // `mount`, not `replaceChildren`: the native call would print a `null`.
    mount(
      box,
      h('div', { class: 'review-row' }, buttons),
      writes
        ? null
        : h('p', { class: 'review-note', id: noteId }, 'Read-only dashboard. Restart it with ', h('code', null, 'engramdb dashboard --allow-writes'), ' to review edges.'),
      live,
    );
    if (focusIndex !== undefined && buttons[focusIndex]) buttons[focusIndex].focus();
  }

  function ask(o, i) {
    const go = h('button', { type: 'button', class: ['btn', 'sm', 'solid', o.tone === 'reject' ? 'danger' : null] }, /^(Confirm|Reject)$/.test(o.label) ? `Yes, ${o.label.toLowerCase()}` : `Confirm: ${o.label}`);
    const back = h('button', { type: 'button', class: ['btn', 'sm', 'ghost'] }, 'Cancel');
    const panel = h('div', { class: 'review-confirm' }, h('p', null, o.consequence), h('div', { class: 'review-row' }, go, back));
    back.addEventListener('click', () => showChoices(i));
    panel.addEventListener('keydown', (ev) => {
      if (ev.key === 'Escape') {
        ev.preventDefault();
        ev.stopPropagation();
        showChoices(i);
      }
    });
    go.addEventListener('click', async () => {
      if (reviews.has(key)) return;
      reviews.set(key, { state: 'pending', text: 'Saving…' });
      go.disabled = true;
      back.disabled = true;
      live.textContent = 'Saving…';
      try {
        const out = await o.send();
        const text = resultText(o, out, edge);
        reviews.set(key, { state: 'done', text });
        live.textContent = text;
        mount(box, live);
        live.focus();
        if (onDone) onDone(out, text);
      } catch (e) {
        reviews.delete(key);
        live.textContent = '';
        const backBtn = h('button', { type: 'button', class: ['btn', 'sm'], onclick: () => showChoices(i) }, 'Back');
        mount(box, h('p', { class: 'review-error', role: 'alert' }, (e && e.message) || String(e)), backBtn);
        backBtn.focus();
      }
    });
    mount(box, panel, live);
    go.focus();
  }

  showChoices();
  return box;
}
