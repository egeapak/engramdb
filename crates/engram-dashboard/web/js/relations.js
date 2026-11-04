// Relation vocabulary shared by every page: the kinds, their names, which
// ones have a direction, and the classes a drawn relation carries. How a
// relation is *drawn* belongs to the graph style (graph/themes/); this module
// only names things, so the memory page, the overview and the review
// controls read the same words whatever style is picked.

export const KIND_LABEL = {
  contradicts: 'Contradicts',
  supersedes: 'Supersedes',
  supports: 'Supports',
  derived_from: 'Derived from',
  related_to: 'Related to',
  duplicates: 'Duplicates',
};

export const KINDS = ['contradicts', 'supersedes', 'supports', 'derived_from', 'related_to', 'duplicates'];

/**
 * Kinds that point from one memory to another. No prototype: a kind is any
 * string a newer binary (or a hand edit) wrote, and `constructor` or
 * `toString` must not read as directed.
 */
export const DIRECTED = Object.freeze({ __proto__: null, supersedes: true, supports: true, derived_from: true });

/** Whether relation kind `k` has a direction (own keys of DIRECTED only). */
export const isDirected = (k) => Object.hasOwn(DIRECTED, k);

export function kindLabel(k) {
  return KIND_LABEL[k] || String(k).replace(/_/g, ' ');
}

/** A drawn relation's classes: kind, status and far-end state. */
export function edgeClass(kind, status, state) {
  return ['cx', 'k-' + String(kind).replace(/[^a-z_]/g, ''), 's-' + String(status || 'confirmed').replace(/[^a-z_]/g, ''), state && state !== 'live' && 'x-' + state];
}
