// The JSON API. Same-origin only, authorized by `Authorization: Bearer`.
//
// The token reaches the page in the URL fragment (`/#token=…`), which a
// browser never sends to a server or puts in a Referer. `takeToken` moves it
// to sessionStorage (this tab, this origin — origins include the port, so
// another server on 127.0.0.1 cannot read it) and strips it from the address
// bar and history with replaceState. There is no cookie: cookies are shared
// by every port on a host.

const TOKEN_KEY = 'engramdb.dashboard.token';
let memoryToken = null; // when sessionStorage is unavailable (some private modes)

function readToken() {
  try {
    return sessionStorage.getItem(TOKEN_KEY) || memoryToken;
  } catch (_) {
    return memoryToken;
  }
}

function forgetToken() {
  memoryToken = null;
  try {
    sessionStorage.removeItem(TOKEN_KEY);
  } catch (_) {
    /* nothing stored */
  }
}

/** Move a `#token=…` fragment into storage and out of the URL. Call first. */
export function takeToken() {
  const m = /^#token=([0-9A-Za-z]+)$/.exec(location.hash);
  if (!m) return;
  memoryToken = m[1];
  try {
    sessionStorage.setItem(TOKEN_KEY, m[1]);
  } catch (_) {
    /* kept in memory for this page */
  }
  history.replaceState(null, '', location.pathname + location.search + '#/');
}

export class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

let csrfToken = null;

/** The per-run CSRF value from /api/session, sent on every non-GET. */
export function setCsrf(v) {
  csrfToken = v;
}

async function request(method, url, body) {
  let res;
  try {
    const headers = { Accept: 'application/json' };
    const token = readToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    const init = { method, credentials: 'omit', headers };
    if (method !== 'GET') {
      headers['Content-Type'] = 'application/json';
      if (csrfToken) headers['X-EngramDB-CSRF'] = csrfToken;
      init.body = JSON.stringify(body || {});
    }
    res = await fetch(url, init);
  } catch (e) {
    throw new ApiError(0, 'offline', 'The dashboard server is not answering. Is `engramdb dashboard` still running?');
  }
  let parsed = null;
  try {
    parsed = await res.json();
  } catch (_) {
    /* a non-JSON refusal (e.g. the host check) */
  }
  if (!res.ok) {
    const err = parsed && parsed.error;
    if (res.status === 401) {
      // A token from an earlier run (same port, same tab) is useless now.
      forgetToken();
      throw new ApiError(401, 'unauthorized', 'This tab has no valid dashboard token. Open the URL `engramdb dashboard` printed — each run has a new token, and it is kept per tab.');
    }
    throw new ApiError(res.status, err ? err.code : 'http', err ? err.message : `HTTP ${res.status}`);
  }
  return parsed;
}

export async function get(path, params) {
  let url = path;
  if (params) {
    const q = new URLSearchParams();
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined && v !== null && v !== '') q.set(k, v);
    }
    const qs = q.toString();
    if (qs) url += '?' + qs;
  }
  return request('GET', url);
}

/** A write. The server refuses it unless started with --allow-writes. */
export function post(path, body) {
  return request('POST', path, body);
}

const enc = encodeURIComponent;

export const api = {
  session: () => get('/api/session'),
  projects: () => get('/api/projects'),
  stats: (id) => get(`/api/projects/${enc(id)}/stats`),
  memories: (id, params) => get(`/api/projects/${enc(id)}/memories`, params),
  memory: (id, mid) => get(`/api/projects/${enc(id)}/memories/${enc(mid)}`),
  doctor: (id) => get(`/api/projects/${enc(id)}/doctor`),
  graph: (id, params) => get(`/api/projects/${enc(id)}/graph`, params),
  confirmEdge: (id, body) => post(`/api/projects/${enc(id)}/edges/confirm`, body),
  rejectEdge: (id, body) => post(`/api/projects/${enc(id)}/edges/reject`, body),
};
