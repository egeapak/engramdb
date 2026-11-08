// The layouts the server computes (src/layout/ in the dashboard crate),
// sent with /graph as `layout` when the page asks (`layout=…`, with the
// box's aspect and width): scope-force (a style's own, or an opened scope's
// by sub-scope), Grid's, and the scope overview. The server is the only
// source of every layout: the page has no layout code, so a layout missing
// from a response that should carry one, or a malformed one, is an error
// the view shows (with a retry), never something it works around.
//
// Wire format (`v` 4). The layout rides in the same JSON document as the
// nodes and edges it lays out, so it is keyed by their order there:
//   scope-force: `pos` (two doubles per entry of `nodes`), `routes` (four
//     per entry of `edges`: both ends), `label_xy` (two per node), each
//     little-endian doubles in base64, and `label_flags` (one byte per node
//     in base64: the anchor, 0 middle / 1 start / 2 end, plus 0x80 when the
//     label found a free spot); `group_keys` + `group` (per node, an index
//     into `group_keys`: the region it was laid out in), `bounds`,
//     `scopes` (the drawn names), `region_r`, and for an opened scope
//     `dirs` (far scope → [dx, dy], its direction on the scope overview).
//   grid: `pos`, `label_xy`, `label_flags` as for scope-force; `routes`
//     every edge's points (two doubles each) in edge order, with
//     `route_points` (one byte per edge, base64: how many points it has);
//     `chains` (per scope: { key, slot, pts }) and `bounds` as plain JSON.
//   scope-overview: plain JSON, `ids` (disc i's scope name) and `pairs`
//     (band k's two scope names) beside `pos`, `r`, `routes`, `labels`,
//     `bounds`: the page finds its discs and bands by name.
// `key` is the server's digest of the width-free input: with the width it
// names this layout, so the page keeps a viewport per layout under it.

/** The encoding this page reads. */
export const SERVER_LAYOUT_VERSION = 4;

const ANCHORS = ['middle', 'start', 'end'];

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

/** A malformed layout: the view shows the message. */
function bad(what) {
  return new Error(`The server's graph layout is malformed (${what}). Reload the page; if it persists, restart \`engramdb dashboard\`.`);
}

/**
 * The layout output (graph/layouts.js's contract) of scope-force from the
 * server's `L`, for the response `g` it came with; `groups` is the
 * grouping the view expects ('top', or 'sub' for an opened scope). Each
 * output also carries `groups` (per node, its region key), `scopes`,
 * `region_r` and `dirs`. Throws on anything malformed.
 */
export function decodeForce(L, g, groups) {
  if (!L || typeof L !== 'object') throw bad('missing');
  if (L.v !== SERVER_LAYOUT_VERSION) throw bad(`version ${L.v}, this page reads ${SERVER_LAYOUT_VERSION}`);
  if (L.name !== 'scope-force') throw bad(`expected scope-force, got ${L.name}`);
  if (L.groups !== groups) throw bad(`grouped by ${L.groups}, expected ${groups}`);
  const n = g.nodes.length;
  const pos = doubles(L.pos);
  const flat = doubles(L.routes);
  const xy = doubles(L.label_xy);
  const flags = bytes(L.label_flags);
  if (pos.length !== 2 * n || xy.length !== 2 * n || flags.length !== n) throw bad('node count');
  if (flat.length !== 4 * g.edges.length) throw bad('relation count');
  if (!pos.every(Number.isFinite) || !flat.every(Number.isFinite) || !xy.every(Number.isFinite)) throw bad('a position is not a number');
  if (!boundsOk(L.bounds) || !finite(L.region_r) || !Array.isArray(L.scopes)) throw bad('bounds');
  if (!L.scopes.every((nm) => nm && typeof nm.key === 'string' && finite(nm.x) && finite(nm.y) && finite(nm.w) && finite(nm.size))) throw bad('scope names');
  const keys = L.group_keys;
  if (!Array.isArray(keys) || !Array.isArray(L.group) || L.group.length !== n) throw bad('groups');
  const labels = new Array(n);
  const groupOf = new Array(n);
  for (let i = 0; i < n; i++) {
    const anchor = ANCHORS[flags[i] & 0x7f];
    if (!anchor) throw bad('label anchor');
    labels[i] = { x: xy[2 * i], y: xy[2 * i + 1], anchor, placed: (flags[i] & 0x80) !== 0 };
    const key = keys[L.group[i]];
    if (typeof key !== 'string') throw bad('groups');
    groupOf[i] = key;
  }
  const routes = new Array(g.edges.length);
  for (let k = 0; k < routes.length; k++) routes[k] = [[flat[4 * k], flat[4 * k + 1]], [flat[4 * k + 2], flat[4 * k + 3]]];
  const dirs = new Map();
  if (L.dirs && typeof L.dirs === 'object') {
    for (const [name, d] of Object.entries(L.dirs)) if (allFinite(d) && d.length === 2) dirs.set(name, d);
  }
  return { pos, routes, labels, bounds: L.bounds, scopes: L.scopes, region_r: L.region_r, groups: groupOf, dirs };
}

/**
 * Grid's layout output from the server's `L` (wire format above), for the
 * response `g` it came with: { pos, routes, labels, bounds, chains }, `chains`
 * (per scope: { key, slot, pts }) for Grid's backdrop. Throws on anything
 * malformed.
 */
export function decodeGrid(L, g) {
  if (!L || typeof L !== 'object') throw bad('missing');
  if (L.v !== SERVER_LAYOUT_VERSION) throw bad(`version ${L.v}, this page reads ${SERVER_LAYOUT_VERSION}`);
  if (L.name !== 'grid') throw bad(`expected grid, got ${L.name}`);
  const n = g.nodes.length;
  const pos = doubles(L.pos);
  const counts = bytes(L.route_points);
  const flat = doubles(L.routes);
  const xy = doubles(L.label_xy);
  const flags = bytes(L.label_flags);
  if (pos.length !== 2 * n || xy.length !== 2 * n || flags.length !== n) throw bad('node count');
  if (counts.length !== g.edges.length) throw bad('relation count');
  if (!pos.every(Number.isFinite) || !flat.every(Number.isFinite) || !xy.every(Number.isFinite)) throw bad('a position is not a number');
  if (!boundsOk(L.bounds)) throw bad('bounds');
  const chainOk = (l) => l && (l.key === null || typeof l.key === 'string') && Array.isArray(l.pts) && l.pts.every((p) => allFinite(p) && p.length === 2);
  if (!Array.isArray(L.chains) || !L.chains.every(chainOk)) throw bad('scope chains');
  const routes = new Array(counts.length);
  let at = 0;
  for (let k = 0; k < counts.length; k++) {
    const m = counts[k];
    if (m < 2 || at + 2 * m > flat.length) throw bad('relation routes');
    const rt = new Array(m);
    for (let j = 0; j < m; j++) rt[j] = [flat[at + 2 * j], flat[at + 2 * j + 1]];
    routes[k] = rt;
    at += 2 * m;
  }
  if (at !== flat.length) throw bad('relation routes');
  const labels = new Array(n);
  for (let i = 0; i < n; i++) {
    const anchor = ANCHORS[flags[i] & 0x7f];
    if (!anchor) throw bad('label anchor');
    labels[i] = { x: xy[2 * i], y: xy[2 * i + 1], anchor, placed: (flags[i] & 0x80) !== 0 };
  }
  return { pos, routes, labels, bounds: L.bounds, chains: L.chains };
}

/**
 * The scope overview's layout from the server's `L`: { pos, r, routes,
 * labels, bounds } with `ids` (disc i's scope name) and `pairs` (band k's
 * two names). Throws on anything malformed.
 */
export function decodeOverview(L) {
  if (!L || typeof L !== 'object') throw bad('missing');
  if (L.v !== SERVER_LAYOUT_VERSION) throw bad(`version ${L.v}, this page reads ${SERVER_LAYOUT_VERSION}`);
  if (L.name !== 'scope-overview') throw bad(`expected scope-overview, got ${L.name}`);
  const n = Array.isArray(L.ids) ? L.ids.length : -1;
  const ok =
    n >= 0 &&
    L.ids.every((id) => typeof id === 'string') &&
    allFinite(L.pos) &&
    L.pos.length === 2 * n &&
    allFinite(L.r) &&
    L.r.length === n &&
    Array.isArray(L.pairs) &&
    Array.isArray(L.routes) &&
    L.routes.length === L.pairs.length &&
    L.pairs.every((p) => Array.isArray(p) && p.length === 2 && typeof p[0] === 'string' && typeof p[1] === 'string') &&
    L.routes.every((rt) => Array.isArray(rt) && rt.length >= 2 && rt.every((p) => allFinite(p) && p.length === 2)) &&
    Array.isArray(L.labels) &&
    L.labels.length === n &&
    L.labels.every((l) => l && finite(l.x) && finite(l.y)) &&
    boundsOk(L.bounds);
  if (!ok) throw bad('overview');
  return { ids: L.ids, pairs: L.pairs, pos: L.pos, r: L.r, routes: L.routes, labels: L.labels, bounds: L.bounds };
}

/** The view's key for the server's layout `L` (its digest and width). */
export function serverKey(L) {
  return `server|${L.key}|${L.width}`;
}
