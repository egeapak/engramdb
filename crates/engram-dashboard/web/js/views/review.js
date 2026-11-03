// Edge review: confirm or reject a proposed edge, with an in-page confirm
// step before anything is sent. The dashboard's only write; the server
// refuses it unless started with --allow-writes, and the buttons say so.
//
// A `contradicts` edge has three ways to be confirmed (the plan's
// "Contradiction direction"): both stand in conflict, or one supersedes the
// other (only the loser's validity window closes) — plus reject, which keeps
// the edge as rejected so no proposer raises it again.

import { h, mount } from '../dom.js';
import { api } from '../api.js';
import { kindLabel } from '../graph/glyphs.js';

function short(text, n = 36) {
  const t = String(text || '').trim();
  return t.length > n ? t.slice(0, n - 1) + '…' : t || 'this memory';
}

/** The choices for one edge, as {label, consequence, send}. */
function choices(edge, viewer, pid) {
  const body = { src: edge.src, kind: edge.kind, dst: edge.dst };
  if (edge.dst_store) body.dst_store = edge.dst_store;
  const srcName = `“${short(edge.src_summary)}”`;
  const dstName = `“${short(edge.dst_summary)}”`;
  if (edge.kind === 'contradicts') {
    const sup = (winner, loserName, label) => ({
      label,
      tone: 'sup',
      consequence: `${loserName} will be marked superseded: its validity window closes and it drops out of default retrieval. The contradiction becomes a supersedes edge.`,
      send: () => api.confirmEdge(pid, { ...body, verdict: 'supersedes', winner }),
    });
    const out = [
      {
        label: 'Both stand (conflict)',
        consequence: 'The contradiction is confirmed. Both memories stay valid, and both carry it.',
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
      consequence: 'The edge is kept as rejected, so it is never proposed again.',
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

/**
 * Review controls for `edge` ({src, dst, dst_store?, kind, status,
 * src_summary?, dst_summary?}). `viewer` is the memory whose page this is,
 * for "this one / the other" wording. `onDone` runs after a successful write.
 */
export function reviewControls(edge, ctx, { viewer, onDone } = {}) {
  if (edge.kind === 'supersedes') return null;
  const writes = !!(ctx.state.session && ctx.state.session.allow_writes);
  const box = h('div', { class: 'review', role: 'group', 'aria-label': `Review: ${kindLabel(edge.kind)}` });
  const live = h('p', { class: 'review-status', role: 'status', 'aria-live': 'polite' });
  const noteId = 'ro-' + Math.random().toString(36).slice(2);

  function showChoices(focusIndex) {
    const opts = choices(edge, viewer, ctx.project.id);
    const buttons = opts.map((o, i) =>
      h(
        'button',
        {
          type: 'button',
          class: ['btn', 'sm', o.tone === 'reject' ? 'ghost' : i === 0 ? 'solid' : null],
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
    const go = h('button', { type: 'button', class: ['btn', 'sm', 'solid', o.tone === 'reject' ? 'danger' : null] }, `Confirm: ${o.label}`);
    const back = h('button', { type: 'button', class: ['btn', 'sm', 'ghost'] }, 'Cancel');
    const panel = h('div', { class: 'review-confirm' }, h('p', null, o.consequence), h('div', { class: 'review-row' }, go, back));
    back.addEventListener('click', () => showChoices(i));
    panel.addEventListener('keydown', (ev) => {
      if (ev.key === 'Escape') {
        ev.preventDefault();
        showChoices(i);
      }
    });
    go.addEventListener('click', async () => {
      go.disabled = true;
      back.disabled = true;
      live.textContent = 'Saving…';
      try {
        await o.send();
        live.textContent = `Done: ${o.label}.`;
        mount(box, live);
        if (onDone) onDone();
      } catch (e) {
        live.textContent = '';
        mount(
          box,
          h('p', { class: 'review-error', role: 'alert' }, (e && e.message) || String(e)),
          h('button', { type: 'button', class: ['btn', 'sm'], onclick: () => showChoices(i) }, 'Back'),
        );
      }
    });
    mount(box, panel, live);
    go.focus();
  }

  showChoices();
  return box;
}
