// Geometry check for the Plain style's relation drawing
// (web/js/graph/themes/plain/relations.js). Exits 1 on a failed check.
//
//   node bench/relations.mjs
//
// Checks, over every kind (including unknown names and inherited-property
// names such as `constructor`), status, length and zoom unit:
//   - the path data has no NaN / Infinity / undefined, and only integers;
//   - a directed kind is wide at x = 0 (the source) and narrow at L;
//   - a kind that is only an inherited property draws like an unknown kind
//     (symmetric, no chevron);
//   - the chevron's tip points toward the target (+x);
//   - the path data is bounded per relation, whatever the length or zoom;
// and times building 2,000 relations at Fit (k ≈ 2), at the view's maximum
// zoom (k ≈ 0.3) and at the unit's floor (k = 0.25), against the dashboard's filter-toggle budget (≤ 100 ms at 2k;
// this is the JS share only, before any DOM work).

import { relationPath, zoomFactor, redrawOnZoom, UNIT } from '../web/js/graph/themes/plain/relations.js';

const KINDS = ['contradicts', 'supersedes', 'supports', 'derived_from', 'related_to', 'duplicates', 'unknown_kind', 'constructor', 'toString', '__proto__'];
const STATUSES = ['confirmed', 'proposed', 'rejected', 'other', undefined, null];
const LENGTHS = [0, 0.4, 1, 3, 12, 40, 200, 1000, 5000];
const UNITS = [0.25, 0.5, 1, 2, 8];
const MAX_D = 16000;

let failures = 0;
const fail = (msg) => {
  failures++;
  if (failures <= 20) console.log('FAIL', msg);
};

/** The polygons of a path: [[x, y], …] per subpath. */
function polys(d) {
  return d
    .split('M')
    .filter(Boolean)
    .map((p) =>
      p
        .replace(/Z$/, '')
        .split('L')
        .map((xy) => xy.split(',').map((v) => Number(v) / UNIT)),
    );
}

/** The largest |y| among points at x ≈ `at` (±0.15) across all subpaths. */
function halfAt(d, at) {
  let m = -1;
  for (const pg of polys(d)) for (const [x, y] of pg) if (Math.abs(x - at) < 0.15) m = Math.max(m, Math.abs(y));
  return m;
}

let worst = 0;
for (const kind of KINDS)
  for (const status of STATUSES)
    for (const L of LENGTHS)
      for (const k of UNITS)
        for (const opts of [{}, { end: 'bar' }, { chevron: true }]) {
          const d = relationPath(kind, status, L, k, opts);
          const tag = `${kind}/${status}/L=${L}/k=${k}/${JSON.stringify(opts)}`;
          if (/NaN|Infinity|undefined|null|\./.test(d)) fail(`${tag}: bad number in ${d.slice(0, 80)}`);
          if (d.length > MAX_D) fail(`${tag}: ${d.length} chars of path data`);
          worst = Math.max(worst, d.length);
          if (L <= 0.5 && d) fail(`${tag}: drew a relation of length ${L}`);
        }

// Direction: a confirmed directed relation is wide at the source.
for (const kind of ['supports', 'supersedes', 'derived_from'])
  for (const L of [12, 40, 200, 1000]) {
    const d = relationPath(kind, 'confirmed', L, 1);
    const a = halfAt(d, 0);
    const b = halfAt(d, L);
    if (!(a > 2 * b)) fail(`${kind} L=${L}: half-width ${a} at the source vs ${b} at the target`);
  }
// Symmetric and unknown kinds (inherited names included) are untapered and chevron-free.
for (const kind of ['related_to', 'duplicates', 'unknown_kind', 'constructor', 'toString', '__proto__']) {
  const d = relationPath(kind, 'proposed', 200, 1, { chevron: true });
  if (Math.abs(halfAt(d, 0) - halfAt(d, 200)) > 0.02 && halfAt(d, 200) >= 0) fail(`${kind}: tapered`);
  if (kind !== 'duplicates' && kind !== 'related_to' && d !== relationPath('unknown_kind', 'proposed', 200, 1, { chevron: true })) fail(`${kind}: not drawn like an unknown kind`);
  if (/,0Z/.test(d)) fail(`${kind}: has a chevron`);
}
// Chevron: its tip (y = 0 vertex) lies past its arms, toward +x.
for (const kind of ['supports', 'supersedes', 'derived_from'])
  for (const st of ['proposed', 'confirmed']) {
    const d = relationPath(kind, st, 200, 1, { chevron: st === 'confirmed' });
    const chev = polys(d).find((pg) => pg.length === 6);
    if (!chev) {
      fail(`${kind}/${st}: no chevron`);
      continue;
    }
    const tip = Math.max(...chev.filter(([, y]) => y === 0).map(([x]) => x));
    const arms = Math.max(...chev.filter(([, y]) => y !== 0).map(([x]) => x));
    if (!(tip > arms)) fail(`${kind}/${st}: chevron points back (tip ${tip}, arms ${arms})`);
  }
// A rejected or long confirmed directed relation has no chevron.
if (polys(relationPath('supports', 'rejected', 200, 1, { chevron: true })).some((pg) => pg.length === 6)) fail('rejected relation has a chevron');
if (polys(relationPath('supports', 'confirmed', 200, 1)).some((pg) => pg.length === 6)) fail('long confirmed relation has a chevron');
// The redraw rule.
if (redrawOnZoom(1, 1.2) || !redrawOnZoom(0.1, 1) || !redrawOnZoom(4, 0.5)) fail('redrawOnZoom thresholds');

// Timing: 2,000 relations, a mix of kinds and statuses, at Fit and max zoom.
function time(k, lo, hi) {
  const kinds = KINDS.slice(0, 6);
  let chars = 0;
  const t0 = performance.now();
  for (let i = 0; i < 2000; i++) {
    const L = lo + ((i * 7919) % (hi - lo));
    chars += relationPath(kinds[i % 6], i % 5 === 0 ? 'proposed' : i % 11 === 0 ? 'rejected' : 'confirmed', L, k, { chevron: i % 3 === 0 }).length;
  }
  return [performance.now() - t0, chars];
}
for (let w = 0; w < 3; w++) time(1, 40, 200);
for (const [name, scale, lo, hi] of [
  ['Fit, large graph', 0.3, 40, 200],
  // The view's deepest zoom: 120 layout units across a 916 px box.
  ['max zoom', 916 / 120, 40, 200],
  ['max zoom, long', 916 / 120, 100, 500],
  ['k floor (scale ≥ 10)', 20, 40, 200],
]) {
  const k = zoomFactor(scale);
  const [ms, chars] = time(k, lo, hi);
  console.log(`2000 relations, ${name} (k=${k.toFixed(2)}, L ${lo}–${hi}): ${ms.toFixed(1)} ms, ${(chars / 1e6).toFixed(2)} MB of path data`);
  if (ms > 100) fail(`${name}: ${ms.toFixed(1)} ms for 2000 relations`);
}
console.log(`largest path data: ${worst} chars (bound ${MAX_D})`);
console.log(failures ? `${failures} check(s) failed` : 'all relation geometry checks passed');
process.exit(failures ? 1 : 0);
