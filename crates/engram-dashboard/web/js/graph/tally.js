// Relation counts under the display filters. The server counts relations by
// kind and status (`kinds: { kind: { status: n } }` on `scope_edges`,
// `stubs` and `stub_scopes`, every kind and status included); the kind /
// proposed / rejected chips are applied here, so toggling one never
// refetches and never moves a disc (the same rule as the memory graph).

/**
 * `{ count, contradictions, proposed, kinds: { kind: n } }` of a server tally
 * under filters `f` ({ kinds, proposed, rejected }, as views/graph.js keeps
 * them). A kind no chip names (a newer binary's) is left out, as the graph
 * leaves out its relations.
 */
export function filtered(tally, f) {
  const out = { count: 0, contradictions: 0, proposed: 0, kinds: {} };
  const kinds = (tally && tally.kinds) || {};
  for (const k of Object.keys(kinds)) {
    if (!f.kinds.includes(k)) continue;
    for (const st of Object.keys(kinds[k])) {
      if (st === 'proposed' && !f.proposed) continue;
      if (st === 'rejected' && !f.rejected) continue;
      const n = kinds[k][st] || 0;
      out.count += n;
      out.kinds[k] = (out.kinds[k] || 0) + n;
      if (k === 'contradicts' && st !== 'rejected') out.contradictions += n;
      if (st === 'proposed') out.proposed += n;
    }
  }
  return out;
}

/** Add server tally `b` into `a` (both `{ count, kinds: { kind: { status: n } } }`). */
export function merge(a, b) {
  a.count = (a.count || 0) + (b.count || 0);
  a.kinds = a.kinds || {};
  for (const k of Object.keys(b.kinds || {})) {
    a.kinds[k] = a.kinds[k] || {};
    for (const st of Object.keys(b.kinds[k])) a.kinds[k][st] = (a.kinds[k][st] || 0) + b.kinds[k][st];
  }
  return a;
}

/** The kind with the most relations in a filtered tally (ties: KINDS order is the caller's). */
export function dominant(ft, order) {
  let best = null;
  for (const k of order) if ((ft.kinds[k] || 0) > 0 && (best === null || ft.kinds[k] > ft.kinds[best])) best = k;
  return best;
}
