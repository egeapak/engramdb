// The hash routes, as pure functions (app.js boots at import, so what a
// node check can call lives here; bench/unit.mjs).
//
// Routes (all hash-based, so the server only ever serves `/`):
//   #/                      overview of the selected project
//   #/p/<id>                overview
//   #/p/<id>/memories?…     memory list (filters in the hash query)
//   #/p/<id>/m/<memory id>  memory detail
//   #/p/<id>/graph?…        memory graph (filters in the hash query)
//   #/p/<id>/relations?…    relation matrix and list (filters in the hash query)
//   #/p/<id>/doctor         store health
//   #/projects              every store on the machine
// A part that is not valid percent-encoding (a hand-edited or cut-off link)
// is the `notfound` view, never an exception.

/** The route a hash names: `{ view, id?, mid?, query }`. */
export function parseHash(hash) {
  const raw = String(hash || '').replace(/^#/, '') || '/';
  const at = raw.indexOf('?');
  const path = at < 0 ? raw : raw.slice(0, at);
  const qs = at < 0 ? '' : raw.slice(at + 1);
  const query = new URLSearchParams(qs);
  let parts;
  try {
    parts = path.split('/').filter(Boolean).map(decodeURIComponent);
  } catch (_) {
    return { view: 'notfound', id: null, query };
  }
  if (parts[0] === 'projects') return { view: 'projects', query };
  if (parts[0] === 'p' && parts[1]) {
    const id = parts[1];
    if (parts[2] === 'memories') return { view: 'memories', id, query };
    if (parts[2] === 'm' && parts[3]) return { view: 'detail', id, mid: parts[3], query };
    if (parts[2] === 'graph') return { view: 'graph', id, query };
    if (parts[2] === 'relations') return { view: 'relations', id, query };
    if (parts[2] === 'doctor') return { view: 'doctor', id, query };
    return { view: 'overview', id, query };
  }
  return { view: 'overview', id: null, query };
}

/** The hash of a view (`extra`: its query, or a memory id for `detail`). */
export function href(view, id, extra) {
  const e = encodeURIComponent;
  switch (view) {
    case 'projects':
      return '#/projects';
    case 'memories':
      return `#/p/${e(id)}/memories${extra ? '?' + extra : ''}`;
    case 'detail':
      return `#/p/${e(id)}/m/${e(extra)}`;
    case 'graph':
      return `#/p/${e(id)}/graph${extra ? '?' + extra : ''}`;
    case 'relations':
      return `#/p/${e(id)}/relations${extra ? '?' + extra : ''}`;
    case 'doctor':
      return `#/p/${e(id)}/doctor`;
    default:
      return `#/p/${e(id)}`;
  }
}

/**
 * Whether a route should refetch the project catalog before it renders:
 * the Projects view always, and a route whose project the catalog does not
 * know or holds as unavailable (a drive mounted since the page loaded, a
 * project registered since).
 */
export function needsCatalog(route, catalog) {
  if (route.view === 'projects') return true;
  if (!route.id || !catalog) return false;
  const p = (catalog.projects || []).find((q) => q.id === route.id);
  return !p || !p.available;
}
