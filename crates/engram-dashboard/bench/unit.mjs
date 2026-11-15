// Unit checks of the frontend's pure functions, in node with no
// dependencies: each check imports an export of web/js/ and compares what
// it returns. A check, not a bench: prints every failure and exits 1 if
// there was one. `node bench/unit.mjs`.
//
// Only modules whose top level touches no browser global can be imported
// here (dom.js reads `document` inside its functions only, so a view that
// imports it still loads). app.js boots at import, which is why its pure
// parts live in route.js.

import { filtered, merge, emptyTally } from '../web/js/graph/tally.js';
import { kindLabel } from '../web/js/relations.js';
import { stepIndex } from '../web/js/graph/walk.js';
import { roundAspect, roundWidth, shortLabel, MAX_LAYOUT_WIDTH } from '../web/js/graph/input.js';
import { fmt, plural, shortId, label, score } from '../web/js/format.js';
import { moreLink } from '../web/js/views/overview.js';

let failed = 0;
let ran = 0;
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** One check: `got` must equal `want` (compared as JSON). */
export function eq(name, got, want) {
  ran++;
  if (!same(got, want)) {
    failed++;
    console.error(`FAIL ${name}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
  }
}

/** One check that `cond` holds. */
export function ok(name, cond, detail) {
  ran++;
  if (!cond) {
    failed++;
    console.error(`FAIL ${name}${detail === undefined ? '' : ': ' + JSON.stringify(detail)}`);
  }
}

// ---------------------------------------------------------------------------
// graph/tally.js
// ---------------------------------------------------------------------------
{
  const all = { kinds: ['contradicts', 'supports', 'related_to'], proposed: true, rejected: true };
  const t = { count: 6, kinds: { contradicts: { proposed: 2, rejected: 1 }, supports: { confirmed: 3 } } };
  eq('filtered: everything on', filtered(t, all), { count: 6, contradictions: 2, proposed: 2, kinds: { contradicts: 3, supports: 3 } });
  eq('filtered: rejected off', filtered(t, { ...all, rejected: false }).count, 5);
  eq('filtered: proposed off', filtered(t, { ...all, proposed: false }).count, 4);
  eq('filtered: a kind no chip names is left out', filtered({ kinds: { future: { confirmed: 2 } } }, all).count, 0);
  eq('filtered: no tally', filtered(undefined, all).count, 0);
  // E14: `counts.hidden_kinds` (kind → status → n) under the chips.
  const hiddenKinds = { kinds: { supports: { confirmed: 2, rejected: 3 }, related_to: { proposed: 1 } } };
  eq('filtered: hidden relations with Rejected off', filtered(hiddenKinds, { ...all, rejected: false }).count, 3);
  eq('filtered: hidden relations with everything on', filtered(hiddenKinds, all).count, 6);
  const a = { count: 1, kinds: { supports: { confirmed: 1 } } };
  merge(a, { count: 2, kinds: { supports: { confirmed: 1, proposed: 1 } } });
  eq('merge adds counts per kind and status', a, { count: 3, kinds: { supports: { confirmed: 2, proposed: 1 } } });

  // E10: a kind is any string; prototype member names are counts too.
  const weird = ['constructor', '__proto__', 'toString'];
  const server = () => JSON.parse('{"count":3,"kinds":{"constructor":{"confirmed":1},"__proto__":{"proposed":1},"toString":{"rejected":1}}}');
  const before = Object.getOwnPropertyNames(Object.prototype).length;
  const m = merge(merge(emptyTally(), server()), server());
  eq('merge: prototype-named kinds are counted', weird.map((k) => m.kinds[k] && Object.values(m.kinds[k])[0]), [2, 2, 2]);
  eq('merge: total', m.count, 6);
  const fw = filtered(m, { kinds: weird, proposed: true, rejected: true });
  eq('filtered: prototype-named kinds', [fw.count, fw.proposed, ...weird.map((k) => fw.kinds[k])], [6, 2, 2, 2, 2]);
  ok('merge and filtered add nothing to Object.prototype', Object.getOwnPropertyNames(Object.prototype).length === before && !Object.hasOwn(Object.prototype, 'confirmed') && !Object.hasOwn(Object, 'confirmed'));
  eq('kindLabel(constructor)', kindLabel('constructor'), 'constructor');
  eq('kindLabel(__proto__)', kindLabel('__proto__'), '  proto  ');
  eq('kindLabel(derived_from)', kindLabel('derived_from'), 'Derived from');
}

// ---------------------------------------------------------------------------
// graph/walk.js (E5)
// ---------------------------------------------------------------------------
{
  const walk = [4, 0, 2, 1, 3];
  eq('stepIndex: ] with nothing focused starts at the first', stepIndex(walk, -1, 1), 0);
  eq('stepIndex: [ with nothing focused starts at the last', stepIndex(walk, -1, -1), 4);
  eq('stepIndex: memory 0 is a memory, not "nothing"', stepIndex(walk, 0, 1), 2);
  eq('stepIndex: ] from the last wraps', stepIndex(walk, 3, 1), 0);
  eq('stepIndex: [ from the first wraps', stepIndex(walk, 4, -1), 4);
  eq('stepIndex: [ from the middle', stepIndex(walk, 2, -1), 1);
  eq('stepIndex: a memory not in the walk starts at an end', stepIndex(walk, 9, 1), 0);
  eq('stepIndex: empty walk', stepIndex([], -1, 1), -1);
}

// ---------------------------------------------------------------------------
// graph/input.js
// ---------------------------------------------------------------------------
{
  eq('roundAspect rounds to 0.05', roundAspect(0.612), 0.6);
  eq('roundAspect clamps low', roundAspect(0.1), 0.3);
  eq('roundAspect clamps high', roundAspect(9), 3);
  eq('roundWidth rounds to 40', roundWidth(1000), 1000);
  eq('roundWidth clamps to the maximum', roundWidth(1e6), MAX_LAYOUT_WIDTH);
  eq('shortLabel cuts at a clause break', shortLabel('Use flock for writes: always'), 'Use flock for writes');
  ok('shortLabel stays short', shortLabel('a very long summary that goes on and on and on').length <= 25);
}

// ---------------------------------------------------------------------------
// format.js
// ---------------------------------------------------------------------------
{
  eq('fmt small integer', fmt(42), '42');
  eq('fmt thousands', fmt(12345), '12,345');
  eq('fmt null', fmt(null), '0');
  eq('plural one', plural(1, 'memory', 'memories'), '1 memory');
  eq('plural many', plural(3, 'memory', 'memories'), '3 memories');
  eq('shortId of a uuid', shortId('0193a6f0-1234-7000-8000-00000000abcd'), '0193a6f0…abcd');
  eq('label', label('needs_review'), 'Needs review');
  for (const v of [null, undefined, NaN, Infinity, 'x', '']) eq(`score(${String(v)})`, score(v), '0.00');
  eq('score(0.5)', score(0.5), '0.50');
  eq('score(1.234)', score(1.234), '1.23');
}

// ---------------------------------------------------------------------------
// views/overview.js
// ---------------------------------------------------------------------------
{
  eq('moreLink proposed_contradiction', moreLink('proposed_contradiction'), ['relations', 'status=proposed&kind=contradicts']);
  eq('moreLink proposed_edge', moreLink('proposed_edge'), ['relations', 'status=proposed']);
  eq('moreLink challenged', moreLink('challenged'), ['memories', 'status=challenged']);
  eq('moreLink needs_review', moreLink('needs_review'), ['memories', 'status=needs_review']);
  eq('moreLink stale', moreLink('stale'), ['memories', 'sort=last_read&dir=asc']);
  eq('moreLink never_read', moreLink('never_read'), ['memories', 'sort=last_read&dir=asc']);
  eq('moreLink unknown', moreLink('future_reason'), null);
}

if (failed) {
  console.error(`${failed} of ${ran} checks failed`);
  process.exit(1);
}
console.log(`${ran} checks passed`);
