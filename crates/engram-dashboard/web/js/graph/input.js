// The layout inputs the page builds from a /graph response, in one module
// because the server builds the very same inputs (src/layout/ in the
// dashboard crate, `build_force_input` / `build_overview_input`) and lays
// them out with its Rust ports. build.rs hashes this file together with
// the layouts it feeds (scope-force.js, scope-overview.js, labels.js,
// radius.js, opened.js, server-layout.js); a Rust test pins that hash to
// `LAYOUT_VERSION`, so an edit here fails CI until the Rust side is checked
// and the version bumped. Everything an input is built from is in those
// files: this module imports nothing unhashed (the overview's scope name
// and count text below are local copies for that reason, not imports of
// scopes.js / format.js).

import { radius, reads } from './radius.js';
import { subKey } from './opened.js';

/** scopes.js `scopeName`, as the overview's labels write it. */
const scopeText = (name) => name || 'No scope';

const NUMBER = new Intl.NumberFormat('en-US');

/** format.js `fmt` for a count (a non-negative integer): "1,234". */
const countText = (n) => (n < 1000 ? String(n) : NUMBER.format(n));

/** The widest box the server lays out (`layout::LayoutRequest`'s cap). */
export const MAX_LAYOUT_WIDTH = 20000;

/**
 * A memory's short label: the summary up to its first clause break, cut on
 * a word boundary. (Titles are not in the index, so the summary is what a
 * graph read can show.)
 */
export function shortLabel(summary, max = 24) {
  let t = String(summary || '').trim();
  const cut = t.search(/[:;—–(]|\.\s|,\s| - /);
  if (cut > 8) t = t.slice(0, cut).trim();
  if (t.length <= max) return t;
  const words = t.slice(0, max).split(/\s+/);
  if (words.length > 1) words.pop();
  return words.join(' ').replace(/[,.:;\s]+$/, '') + '…';
}

/** The box's aspect as a layout input rounds it (to 0.05, within 0.3–3). */
export const roundAspect = (a) => Math.round(Math.min(3, Math.max(0.3, a)) * 20) / 20;

/** The box's width as a layout input rounds it (to 40 px, at most MAX_LAYOUT_WIDTH). */
export const roundWidth = (w) => Math.round(Math.min(MAX_LAYOUT_WIDTH, w) / 40) * 40;

/**
 * The graph layout's input for `g` (a /graph response): node order as
 * sent, each node's group key its top scope (or, for an opened scope, its
 * sub-scope), edge ends as node indices (-1: a stub off the source).
 * `labels` are the nodes' short labels, when the caller already has them.
 */
export function forceInput(g, { layout, options, opened, aspect, width, labels }) {
  const nodes = g.nodes;
  const byId = new Map();
  for (let i = 0; i < nodes.length; i++) byId.set(nodes[i].id, i);
  const groupKeys = new Array(nodes.length);
  const inNodes = new Array(nodes.length);
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i];
    const group = opened ? subKey(n) : n.top_scope || '';
    groupKeys[i] = group;
    inNodes[i] = { id: n.id, top_scope: group, slot: n.color_slot ?? null, scope: n.scope || '', degree: n.degree, r: radius(n), label: labels ? labels[i] : shortLabel(n.summary), weight: reads(n) };
  }
  const edges = new Array(g.edges.length);
  for (let k = 0; k < g.edges.length; k++) {
    const e = g.edges[k];
    const ti = e.dst_store || !byId.has(e.dst) ? -1 : byId.get(e.dst);
    // A stub's far end is not in the node set, so its scope is unknown
    // (null) unless the server names it.
    // A source that is not a drawn node is -1, as the server writes it
    // (`undefined` would mix into inputCheck as 0, the server's -1 as ~0).
    const si = byId.has(e.src) ? byId.get(e.src) : -1;
    edges[k] = { s: si, t: ti, kind: e.kind, far_scope: ti >= 0 ? nodes[ti].top_scope || '' : (e.far_scope ?? null) };
  }
  const input = { layout, nodes: inNodes, options: options || {}, edges, aspect: roundAspect(aspect) };
  if (width != null) input.width = roundWidth(width);
  return { input, groupKeys, byId };
}

/**
 * Pairs of different scopes from a response's `scope_edges` rows ({ a, b,
 * count }; a directed kind has one row per direction): unordered (`a` <
 * `b`), in order of first appearance, `n` the summed count and `rows` the
 * rows summed. The overview's edge k is pair k, so this order is part of
 * the layout input (`layout::build_overview_input` ports it).
 */
export function scopePairs(rows) {
  const pairs = new Map();
  for (const row of rows || []) {
    if (row.a === row.b) continue;
    const [a, b] = row.a < row.b ? [row.a, row.b] : [row.b, row.a];
    const key = JSON.stringify([a, b]);
    if (!pairs.has(key)) pairs.set(key, { a, b, n: 0, rows: [] });
    const p = pairs.get(key);
    p.n += row.count || 0;
    p.rows.push(row);
  }
  return [...pairs.values()];
}

/**
 * The scope overview's input: one disc per scope with memories (`scopes`,
 * the response's), bands between `pairs` ({ a, b, n }: unordered pairs of
 * different scopes, `n` relations each).
 */
export function overviewInput(scopes, pairs, width, aspect) {
  const index = new Map(scopes.map((l, i) => [l.name, i]));
  return {
    layout: 'scope-overview',
    nodes: scopes.map((l) => ({ id: l.name, count: l.count, label: `${scopeText(l.name)} · ${countText(l.count)}`, name: scopeText(l.name) })),
    edges: pairs.map((p) => ({ s: index.get(p.a), t: index.get(p.b), n: p.n })),
    aspect: roundAspect(aspect),
    width: roundWidth(width),
  };
}

/**
 * A cheap checksum of what a graph layout reads of its input (FNV-1a over
 * 32-bit words: each node's id and group key as UTF-16 units, its radius
 * in thousandths and its label's length, then every edge's two ends).
 * The server sends the same sum of the input it laid out
 * (`layout::input_check`), so a layout of another input is never drawn.
 */
export function inputCheck(input) {
  // Straight loops (no closures): this runs once per drawn graph, cold.
  let h = 0x811c9dc5;
  const nodes = input.nodes;
  for (let i = 0; i < nodes.length; i++) {
    const nd = nodes[i];
    const id = nd.id;
    for (let k = 0; k < id.length; k++) h = Math.imul(h ^ id.charCodeAt(k), 16777619);
    h = Math.imul(h ^ 0x10000, 16777619);
    const top = nd.top_scope;
    for (let k = 0; k < top.length; k++) h = Math.imul(h ^ top.charCodeAt(k), 16777619);
    h = Math.imul(h ^ 0x10000, 16777619);
    h = Math.imul(h ^ Math.round(nd.r * 1000), 16777619);
    h = Math.imul(h ^ (nd.label || '').length, 16777619);
  }
  const edges = input.edges;
  for (let k = 0; k < edges.length; k++) {
    h = Math.imul(h ^ edges[k].s, 16777619);
    h = Math.imul(h ^ edges[k].t, 16777619);
  }
  return h >>> 0;
}

/**
 * `inputCheck` for the overview's input: each disc's id (UTF-16 units),
 * count and both label lengths, then every band's ends and count. The
 * server sends the same (`layout::overview_check`).
 */
export function overviewCheck(input) {
  let h = 0x811c9dc5;
  const nodes = input.nodes;
  for (let i = 0; i < nodes.length; i++) {
    const nd = nodes[i];
    const id = nd.id;
    for (let k = 0; k < id.length; k++) h = Math.imul(h ^ id.charCodeAt(k), 16777619);
    h = Math.imul(h ^ 0x10000, 16777619);
    h = Math.imul(h ^ nd.count, 16777619);
    h = Math.imul(h ^ nd.label.length, 16777619);
    h = Math.imul(h ^ nd.name.length, 16777619);
  }
  const edges = input.edges;
  for (let k = 0; k < edges.length; k++) {
    h = Math.imul(h ^ edges[k].s, 16777619);
    h = Math.imul(h ^ edges[k].t, 16777619);
    h = Math.imul(h ^ edges[k].n, 16777619);
  }
  return h >>> 0;
}
