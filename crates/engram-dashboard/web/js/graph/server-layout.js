// Layouts the server computed (src/layout/ in the dashboard crate): the
// Rust ports of scope-force.js and scope-overview.js, sent with /graph as
// `layout` when the page asks (`layout=…`, with the box's aspect and width).
// They are the same bits the worker would compute for the same input (a
// Rust test compares them with this JS on committed fixtures), so the page
// uses one in place of the worker when it is for exactly the input the page
// built; anything else (no layout, another version, another grouping,
// another input, a malformed field) returns null and the page lays out as
// before: in its worker, or for the overview on the main thread.
//
// Encoding (`v` 3): the header names the layout, its grouping (`groups`:
// top or sub-scopes), the input's node and edge counts, aspect, width and
// `check` (graph/input.js `inputCheck`, or `overviewCheck` for the
// overview, recomputed here). scope-force's
// positions are little-endian doubles in base64, one byte per label (the
// spot index into labels.js's `spotsAt`, rebuilt with `spotAt`; 255 for
// unplaced), the stub ends
// as doubles, the rest JSON. The overview is plain JSON in the layout's own
// output shape. `key` is the server's digest of the width-free input: with
// the width it names this layout, so the page caches it under that.

import { spotAt, labelWidth } from './labels.js';
import { inputCheck, overviewCheck } from './input.js';

/** The encoding this page reads. */
export const SERVER_LAYOUT_VERSION = 3;

/** The layouts the server offers. */
export const SERVER_LAYOUTS = Object.freeze({ 'scope-force': true, 'scope-overview': true });

/** Whether this machine stores doubles little-endian (every current one). */
const LE = new Uint8Array(new Float64Array([1]).buffer)[7] === 0x3f;

function bytes(b64) {
  if (typeof Uint8Array.fromBase64 === 'function') return Uint8Array.fromBase64(b64 || '');
  const bin = atob(b64 || '');
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Little-endian doubles: a view of the decoded bytes (layout code only indexes them). */
function doubles(b64) {
  const u = bytes(b64);
  if (u.length % 8) throw new Error('not doubles');
  if (LE && u.byteOffset % 8 === 0) return new Float64Array(u.buffer, u.byteOffset, u.length >> 3);
  const dv = new DataView(u.buffer, u.byteOffset, u.length);
  const out = new Float64Array(u.length >> 3);
  for (let i = 0; i < out.length; i++) out[i] = dv.getFloat64(8 * i, true);
  return out;
}

const finite = (v) => typeof v === 'number' && Number.isFinite(v);
const allFinite = (a) => Array.isArray(a) && a.every(finite);
const boundsOk = (b) => !!b && finite(b.x) && finite(b.y) && finite(b.w) && finite(b.h);

/** Whether `L`'s header is for `input` (its `check` too, and for scope-force `groups`). */
export function isFor(L, input, groups = 'top') {
  if (!L || typeof L !== 'object') return false;
  const head =
    L.v === SERVER_LAYOUT_VERSION &&
    L.name === input.layout &&
    L.n === (input.nodes || []).length &&
    L.e === (input.edges || []).length &&
    L.aspect === input.aspect &&
    L.width === input.width &&
    Object.keys(input.options || {}).length === 0;
  if (!head) return false;
  if (L.name !== 'scope-force') return L.check === overviewCheck(input);
  return L.groups === groups && L.check === inputCheck(input);
}

/** The page's cache key for the server's layout `L` (null: none usable). */
export function serverKey(L) {
  return L && typeof L.key === 'string' ? `server|${L.key}|${L.width}` : null;
}

function overviewOf(L, input) {
  const n = input.nodes.length;
  const ok =
    allFinite(L.pos) &&
    L.pos.length === 2 * n &&
    allFinite(L.r) &&
    L.r.length === n &&
    Array.isArray(L.routes) &&
    L.routes.length === input.edges.length &&
    L.routes.every((rt) => Array.isArray(rt) && rt.length >= 2 && rt.every((p) => allFinite(p) && p.length === 2)) &&
    Array.isArray(L.labels) &&
    L.labels.length === n &&
    L.labels.every((l) => l && finite(l.x) && finite(l.y)) &&
    boundsOk(L.bounds);
  return ok ? { pos: L.pos, r: L.r, routes: L.routes, labels: L.labels, bounds: L.bounds } : null;
}

function forceOf(L, input) {
  const n = input.nodes.length;
  const pos = doubles(L.pos);
  const spots = bytes(L.labels);
  const stubs = doubles(L.stubs);
  if (pos.length !== 2 * n || spots.length !== n || !pos.every(Number.isFinite) || !stubs.every(Number.isFinite)) return null;
  if (!boundsOk(L.bounds) || !finite(L.region_r) || !Array.isArray(L.scopes)) return null;
  if (!L.scopes.every((nm) => nm && typeof nm.key === 'string' && finite(nm.x) && finite(nm.y) && finite(nm.w) && finite(nm.size))) return null;
  const labels = new Array(n);
  for (let i = 0; i < n; i++) {
    const nd = input.nodes[i];
    const placed = spots[i] !== 255;
    const w = labelWidth(nd.label || '');
    const sp = spotAt(pos[2 * i], pos[2 * i + 1], (nd.r || 6) + 4, placed ? spots[i] : 0);
    if (!sp) return null;
    labels[i] = { x: sp.x, y: sp.y, w, anchor: sp.anchor, placed };
  }
  let j = 0;
  const routes = new Array(input.edges.length);
  for (let k = 0; k < routes.length; k++) {
    const e = input.edges[k];
    const s = e.s;
    if (!(s >= 0 && s < n)) {
      routes[k] = [[0, 0], [0, 0]];
      continue;
    }
    const a = [pos[2 * s], pos[2 * s + 1]];
    if (e.t >= 0 && e.t < n) routes[k] = [a, [pos[2 * e.t], pos[2 * e.t + 1]]];
    else {
      routes[k] = [a, [stubs[2 * j], stubs[2 * j + 1]]];
      j++;
    }
  }
  if (2 * j !== stubs.length) return null;
  return { pos, routes, labels, bounds: L.bounds, scopes: L.scopes, region_r: L.region_r };
}

/**
 * The layout output (graph/layouts.js's contract) from the server's `L`
 * for `input` (`groups`: the grouping the page laid out by, top or sub),
 * or null when `L` is not for this input or is malformed.
 */
export function fromServer(L, input, groups = 'top') {
  return isFor(L, input, groups) ? decode(L, input) : null;
}

/** `fromServer` for a header `isFor` already accepted. */
export function decode(L, input) {
  try {
    return L.name === 'scope-overview' ? overviewOf(L, input) : forceOf(L, input);
  } catch (_) {
    return null;
  }
}
