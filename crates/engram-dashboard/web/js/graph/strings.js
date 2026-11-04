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
    `Graph of ${project}: ${plural(n, 'memory', 'memories')}. Arrow keys pan, plus and minus zoom, 0 fits, ] and [ step through memories (through the selected memory's relations once one is selected), Enter selects, Escape clears the selection.`,
  toolsHint: 'Drag to pan · click a memory to select it, or a relation to pin it',
  wheelHint: 'Click the graph (or hold Ctrl/⌘) to zoom with the wheel',
  styleLabel: 'Style',
  styleAria: 'Graph style',
  cardTitle: 'Select a memory or relation',
  cardHint: 'Hover a memory to show its relations and name its neighbours. Click it to select it: this panel then states its relations in words, and proposed ones are reviewed here. Click a relation to pin it alone.',
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
  // The scope overview (views/graph-overview.js) and an opened scope.
  overviewIntro: (n) => [`${plural(n, 'memory', 'memories')}, shown by top-level scope: each disc is a scope, each band the relations between two.`, 'Open a scope to see its memories.'],
  overviewAria: (project, n) => `Scopes of ${project}: ${plural(n, 'top-level scope')}. Tab to a scope and press Enter to open it.`,
  overviewHint: 'Click a scope to open it',
  overviewNote: (n, scopes, unscoped, min) =>
    `${plural(n, 'memory', 'memories')} in ${plural(scopes, 'top-level scope')}${unscoped ? ` (${unscoped} with no scope)` : ''}: more than ${min}, so the graph opens on its scopes. Band width follows relations; ⚠ counts contradictions.`,
  overviewHidden: (n, cut) =>
    `${plural(n, 'scope pair')} with under ${plural(cut, 'relation')} not drawn (one with a contradiction keeps a dashed red hairline and its ⚠).`,
  overviewCardTitle: 'Scope overview',
  overviewCardHint: 'Hover a scope to highlight its bands; click it to open its memories and relations. "Show all memories" draws the whole graph (at most 800 memories).',
  overviewPairs: 'Relations between scopes',
  overviewNoPairs: 'No relations between scopes with these filters.',
  overviewAllPairs: (n) => `Show all ${plural(n, 'scope pair')}`,
  overviewFewerPairs: 'Show fewer scope pairs',
  showAll: 'Show all memories',
  legendOverview: 'Scope overview',
  legendDisc: 'A top-level scope: area follows its memories',
  legendBand: 'Relations between two scopes: width follows how many',
  legendPill: 'Contradictions between two scopes (always shown)',
  legendHair: 'Two scopes with few relations, one of them a contradiction',
  crumbsAria: 'Graph scope',
  crumbAll: 'All scopes',
  tagAria: (name, n, c) => `${name}: ${plural(n, 'relation')} to this scope${c ? `, ${plural(c, 'contradiction')}` : ''}. Open ${name}.`,
  // The details panel of a selected memory (graph/details.js). Relation
  // groups, read from the selected memory: its own direction first, the
  // other end's second (graph/details.js's PANEL_GROUPS).
  relContradicts: 'Contradicts',
  relSupports: 'Supports',
  relSupportedBy: 'Supported by',
  relSupersedes: 'Supersedes',
  relSupersededBy: 'Superseded by',
  relDerivedFrom: 'Derived from',
  relSourceOf: 'Source of',
  relDuplicates: 'Duplicates',
  relRelatedTo: 'Related to',
  // A kind this page has no words for (a newer binary's): its name, and
  // which way it points when it has a direction.
  relOther: (name, incoming) => (incoming ? `${name} (from the other memory)` : name),
  panelEyebrow: 'Memory · selected',
  panelClear: 'Clear',
  panelClearAria: 'Clear the selection',
  panelOpen: 'Open memory →',
  panelType: 'Type',
  panelScope: 'Scope',
  panelNoScope: 'No logical scope',
  panelCriticality: 'Criticality',
  panelStatus: 'Status',
  panelReads: 'Reads',
  panelReadsValue: (r, i, f) => `${r} retrieved · ${i} injected · ${f} fetched`,
  panelRelations: (n) => `Relations (${n})`,
  panelNone: 'No relations shown. Check the filters.',
  panelSameScope: 'same scope',
  panelProposed: 'Proposed',
  panelRejected: 'Rejected',
  panelDangling: 'dangling: the other memory was deleted',
  panelUnknown: 'not checked: in a store this dashboard could not open',
  panelOtherStore: 'in another store',
  panelInvalidated: 'its validity window closed',
  panelOtherScopes: 'In other scopes',
  panelScopeRow: (name, n) => `${name} · ${plural(n, 'relation')}`,
  panelScopeAria: (name, n, c) => `${plural(n, 'relation')} to ${name}${c ? `, ${plural(c, 'contradiction')}` : ''}. Open ${name}.`,
  panelReadOnly: 'Read-only dashboard: restart it with engramdb dashboard --allow-writes to review proposed relations here.',
  panelKeys: '] and [ step through these relations, Enter selects the memory (or opens the scope), Escape clears the selection.',
  panelSelectAria: (summary) => `Select ${summary}`,
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
