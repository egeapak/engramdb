// The JSON API. Same-origin only; the session cookie rides along.

export class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
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
  let res;
  try {
    res = await fetch(url, { credentials: 'same-origin', headers: { Accept: 'application/json' } });
  } catch (e) {
    throw new ApiError(0, 'offline', 'The dashboard server is not answering. Is `engramdb dashboard` still running?');
  }
  let body = null;
  try {
    body = await res.json();
  } catch (_) {
    /* a non-JSON refusal (e.g. the host check) */
  }
  if (!res.ok) {
    const err = body && body.error;
    if (res.status === 401) {
      throw new ApiError(401, 'unauthorized', 'This page is not signed in. Open the URL `engramdb dashboard` printed — each run has a new token.');
    }
    throw new ApiError(res.status, err ? err.code : 'http', err ? err.message : `HTTP ${res.status}`);
  }
  return body;
}

const enc = encodeURIComponent;

export const api = {
  session: () => get('/api/session'),
  projects: () => get('/api/projects'),
  stats: (id) => get(`/api/projects/${enc(id)}/stats`),
  memories: (id, params) => get(`/api/projects/${enc(id)}/memories`, params),
  memory: (id, mid) => get(`/api/projects/${enc(id)}/memories/${enc(mid)}`),
  doctor: (id) => get(`/api/projects/${enc(id)}/doctor`),
};
