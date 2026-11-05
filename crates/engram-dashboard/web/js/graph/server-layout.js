// Layouts the server computed (src/layout/ in the dashboard crate): the
// Rust ports of scope-force.js and scope-overview.js, sent with /graph as
// `layout` when the page asks (`layout=…`, with the box's aspect and width).
// They are the same bits the worker would compute for the same input, so
// the page uses one in place of the worker when it is for exactly the input
// the page built; anything else (no layout, another version, another
// shape) returns null and the page lays out as before.
//
// Encoding (`v` 1): scope-force's positions are little-endian doubles in
// base64, one byte per label (the spot index into labels.js's `spotsAt`,
// 255 for unplaced), the stub ends as doubles, the rest JSON. The overview
// is plain JSON in the layout's own output shape.

import { spotsAt, labelWidth } from './labels.js';

/** The encoding this page reads. */
export const SERVER_LAYOUT_VERSION = 1;

/** The layouts the server offers. */
export const SERVER_LAYOUTS = Object.freeze({ 'scope-force': true, 'scope-overview': true });

function bytes(b64) {
  const bin = atob(b64 || '');
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function doubles(b64) {
  const u = bytes(b64);
  const dv = new DataView(u.buffer);
  const out = new Array(u.length >> 3);
  for (let i = 0; i < out.length; i++) out[i] = dv.getFloat64(8 * i, true);
  return out;
}

/** Whether `L` is the server's layout of exactly `input` (its header). */
function matches(L, input) {
  return (
    !!L &&
    L.v === SERVER_LAYOUT_VERSION &&
    L.name === input.layout &&
    L.n === (input.nodes || []).length &&
    L.e === (input.edges || []).length &&
    L.aspect === input.aspect &&
    L.width === input.width &&
    Object.keys(input.options || {}).length === 0
  );
}

/**
 * The layout output (graph/layouts.js's contract) from the server's `L`
 * for `input`, or null when `L` is not for this input.
 */
export function fromServer(L, input) {
  if (!matches(L, input)) return null;
  try {
    if (L.name === 'scope-overview') return { pos: L.pos, r: L.r, routes: L.routes, labels: L.labels, bounds: L.bounds };
    const n = input.nodes.length;
    const pos = doubles(L.pos);
    const spots = bytes(L.labels);
    const stubs = doubles(L.stubs);
    if (pos.length !== 2 * n || spots.length !== n) return null;
    const labels = input.nodes.map((nd, i) => {
      const placed = spots[i] !== 255;
      const w = labelWidth(nd.label || '');
      const sp = spotsAt(pos[2 * i], pos[2 * i + 1], (nd.r || 6) + 4, w, true)[placed ? spots[i] : 0];
      return { x: sp.x, y: sp.y, w, anchor: sp.anchor, placed };
    });
    let j = 0;
    const routes = input.edges.map((e) => {
      const s = e.s;
      if (!(s >= 0 && s < n)) return [[0, 0], [0, 0]];
      const a = [pos[2 * s], pos[2 * s + 1]];
      if (e.t >= 0 && e.t < n) return [a, [pos[2 * e.t], pos[2 * e.t + 1]]];
      const end = [stubs[2 * j], stubs[2 * j + 1]];
      j++;
      return [a, end];
    });
    if (2 * j !== stubs.length) return null;
    return { pos, routes, labels, bounds: L.bounds, scopes: L.scopes, region_r: L.region_r };
  } catch (_) {
    return null;
  }
}
