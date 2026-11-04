// The graph view's own words, by key. Core text goes through `t(key, …)`,
// which asks the picked style first (`strings` in its module) and falls back
// to these, so a style can add flavour without the core ever naming it. The
// core words are neutral: graph, memory, relation, scope (a Rust test scans
// every core asset for the retired metaphors).
//
// A value is a string or a function of the arguments the call site passes.

import { plural } from '../format.js';

export const CORE = {
  /** The header's light/dark button, by whether the page is dark now. */
  schemeToggle: (dark) => (dark ? 'Light' : 'Dark'),
  schemeToggleAria: (dark) => (dark ? 'Switch to the light theme' : 'Switch to the dark theme'),
  title: (project) => `${project} graph`,
  intro: () => ['Each memory is shaped by its type and grouped by its top-level scope.', 'Size follows reads.'],
  loading: 'Loading graph…',
  layingOut: (n) => `Laying out ${plural(n, 'memory', 'memories')}…`,
  empty: 'No memories to draw yet.',
  emptyScope: 'No memories in this top-level scope with these filters.',
  graphAria: (project, n) =>
    `Graph of ${project}: ${plural(n, 'memory', 'memories')}. Arrow keys pan, plus and minus zoom, 0 fits, ] and [ step through memories, Enter pins one in the details card.`,
  toolsHint: 'Drag to pan · click a memory or relation to pin it',
  wheelHint: 'Click the graph (or hold Ctrl/⌘) to zoom with the wheel',
  styleLabel: 'Style',
  styleAria: 'Graph style',
  cardTitle: 'Select a memory or relation',
  cardHint: 'Hover a memory to highlight its relations. Click it, or a relation, to pin it here: a proposed relation is reviewed from this card.',
  legendScopes: 'Top-level scopes',
  legendMoreScopes: (n) => `and ${plural(n, 'more scope')}`,
  legendMemories: 'Memories (by type)',
  legendHub: '2+ relations',
  legendNever: 'Never read',
  legendChallenged: 'Challenged',
  legendSize: 'Size follows reads',
  legendRelations: 'Relations',
  // Only for a style that tapers relations (buildLegend's `direction`).
  legendDirection: 'Wide end: the memory that supports, supersedes or is derived from the other',
  legendProposedContradiction: 'Proposed contradiction',
  legendProposedOther: 'Any other proposed relation',
  legendRejected: 'Rejected',
  legendDeleted: 'To a deleted memory',
  legendUnknown: 'Into a store not checked here',
  // Notes above the graph: a style that marks these ends says how.
  noteDangling: (n) => `${plural(n, 'relation')} to a deleted memory`,
  noteUnknown: (n) => `${plural(n, 'relation')} into a store that cannot be checked here`,
};

/** `t(key, …args)` for a style: its own string when it has one, else the core's. */
export function makeT(style) {
  const own = (style && style.strings) || {};
  return (key, ...args) => {
    const v = Object.prototype.hasOwnProperty.call(own, key) ? own[key] : CORE[key];
    if (v === undefined) return key;
    return typeof v === 'function' ? v(...args) : v;
  };
}
