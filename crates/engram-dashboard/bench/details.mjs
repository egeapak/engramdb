// The details panel's direction words, from both ends of every relation
// kind (web/js/graph/details.js `groupRelations`). A check, not a bench:
// exits 1 on the first wrong label. `node bench/details.mjs`.
import { groupRelations, PANEL_GROUPS } from '../web/js/graph/details.js';
import { makeT } from '../web/js/graph/strings.js';
import { KINDS } from '../web/js/relations.js';

const t = makeT(null);
const SYMMETRIC = new Set(['contradicts', 'related_to', 'duplicates']);
// kind → [what A (the source) reads, what B (the target) reads]
const WANT = {
  contradicts: ['Contradicts', 'Contradicts'],
  supports: ['Supports', 'Supported by'],
  supersedes: ['Supersedes', 'Superseded by'],
  derived_from: ['Derived from', 'Source of'],
  duplicates: ['Duplicates', 'Duplicates'],
  related_to: ['Related to', 'Related to'],
  future_kind: ['future kind', 'future kind (from the other memory)'],
};
let bad = 0;
for (const kind of [...KINDS, 'future_kind']) {
  const g = {
    nodes: [{ id: 'a', summary: 'A' }, { id: 'b', summary: 'B' }],
    edges: [{ src: 'a', dst: 'b', kind, status: 'confirmed', symmetric: SYMMETRIC.has(kind), state: 'live' }],
  };
  const byId = new Map([['a', 0], ['b', 1]]);
  for (const [i, side] of [[0, 'source'], [1, 'target']]) {
    const groups = groupRelations(g, byId, i, [0], t);
    const got = groups.map((gr) => gr.label).join('|');
    const want = WANT[kind][i];
    const other = groups[0] && groups[0].items[0] && groups[0].items[0].j;
    if (got !== want || other !== 1 - i) {
      console.error(`${kind} at the ${side}: got ${JSON.stringify(got)} (other end ${other}), want ${JSON.stringify(want)}`);
      bad++;
    }
  }
}
// Order: contradictions first, a directed kind's own direction before the other's.
const g = {
  nodes: [{ id: 'm', summary: 'M' }, ...'abcdefgh'.split('').map((id) => ({ id, summary: id }))],
  edges: [
    { src: 'a', dst: 'm', kind: 'supports', symmetric: false },
    { src: 'm', dst: 'b', kind: 'related_to', symmetric: true },
    { src: 'm', dst: 'c', kind: 'supports', symmetric: false },
    { src: 'd', dst: 'm', kind: 'contradicts', symmetric: true },
    { src: 'e', dst: 'm', kind: 'derived_from', symmetric: false },
    { src: 'm', dst: 'f', kind: 'supersedes', symmetric: false },
  ].map((e) => ({ status: 'confirmed', state: 'live', ...e })),
};
const byId = new Map(g.nodes.map((n, i) => [n.id, i]));
const order = groupRelations(g, byId, 0, g.edges.map((_, k) => k), t).map((gr) => gr.label).join(', ');
const wantOrder = 'Contradicts, Supports, Supported by, Supersedes, Source of, Related to';
if (order !== wantOrder) {
  console.error(`order: got ${order}, want ${wantOrder}`);
  bad++;
}
if (PANEL_GROUPS.length !== KINDS.length) {
  console.error(`PANEL_GROUPS has ${PANEL_GROUPS.length} rows for ${KINDS.length} kinds`);
  bad++;
}
if (bad) process.exit(1);
console.log(`details panel: ${KINDS.length + 1} kinds from both ends and the group order are right`);
