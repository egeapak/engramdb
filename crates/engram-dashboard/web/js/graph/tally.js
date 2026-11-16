// Relation counts under the display filters. The server counts relations by
// kind and status (`kinds: { kind: { status: n } }` on `scope_edges`,
// `stubs` and `stub_scopes`, every kind and status included); the kind /
// proposed / rejected chips are applied here, so toggling one never
// refetches and never moves a disc (the same rule as the memory graph).

// Every map here is keyed by a relation kind or a status, which is any
// string a newer binary or a hand edit wrote: `constructor`, `toString` or
// `__proto__` must be a count like any other, never an `Object.prototype`
// member. So the maps have no prototype, and the server's are read through
// `Object.hasOwn` only.

const own = (o, k) => o !== null && typeof o === 'object' && Object.hasOwn(o, k);
const ownKeys = (o) => (o !== null && typeof o === 'object' ? Object.keys(o) : []);

/** An empty tally to `merge` into: `{ count: 0, kinds: {} }` with no prototype on `kinds`. */
export function emptyTally() {
  return { count: 0, kinds: Object.create(null) };
}

/**
 * `{ count, contradictions, proposed, kinds: { kind: n } }` of a server tally
 * under filters `f` ({ kinds, proposed, rejected }, as views/graph.js keeps
 * them). A kind no chip names (a newer binary's) is left out, as the graph
 * leaves out its relations.
 */
export function filtered(tally, f) {
  const out = { count: 0, contradictions: 0, proposed: 0, kinds: Object.create(null) };
  const kinds = tally && own(tally, 'kinds') ? tally.kinds : null;
  for (const k of ownKeys(kinds)) {
    if (!f.kinds.includes(k)) continue;
    const per = kinds[k];
    for (const st of ownKeys(per)) {
      if (st === 'proposed' && !f.proposed) continue;
      if (st === 'rejected' && !f.rejected) continue;
      const n = Number(per[st]) || 0;
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
  a.count = (a.count || 0) + ((b && b.count) || 0);
  if (!a.kinds || Object.getPrototypeOf(a.kinds) !== null) a.kinds = Object.assign(Object.create(null), a.kinds || {});
  const bk = b && own(b, 'kinds') ? b.kinds : null;
  for (const k of ownKeys(bk)) {
    const into = a.kinds[k] || (a.kinds[k] = Object.create(null));
    for (const st of ownKeys(bk[k])) into[st] = (into[st] || 0) + (Number(bk[k][st]) || 0);
  }
  return a;
}

/** The kind with the most relations in a filtered tally (ties: KINDS order is the caller's). */
export function dominant(ft, order) {
  let best = null;
  for (const k of order) if ((ft.kinds[k] || 0) > 0 && (best === null || ft.kinds[k] > ft.kinds[best])) best = k;
  return best;
}
