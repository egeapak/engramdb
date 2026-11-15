// Entry module: session, theme, project switcher and hash routing (the
// routes themselves: route.js).

import { $, $$, h, mount } from './dom.js';
import { api, takeToken, setCsrf } from './api.js';
import { renderOverview } from './views/overview.js';
import { renderMemories } from './views/memories.js';
import { renderDetail } from './views/detail.js';
import { renderGraph } from './views/graph.js';
import { renderRelations } from './views/relations.js';
import { renderDoctor } from './views/doctor.js';
import { renderProjects } from './views/projects.js';
import { errorBox } from './views/common.js';
import { parseHash, href, needsCatalog } from './route.js';
import { currentStyle, onStyleChange, useStyle } from './graph/themes/index.js';
import { makeT } from './graph/strings.js';

const state = {
  session: null,
  catalog: null,
  project: null, // selected project id
};

const THEME_KEY = 'engramdb.dashboard.theme';

/** Whether the graph view is shown: only there may a style flavour the header. */
let onGraph = false;

function storedTheme() {
  try {
    return localStorage.getItem(THEME_KEY);
  } catch (_) {
    return null;
  }
}

function effectiveTheme() {
  const t = document.documentElement.getAttribute('data-theme');
  if (t) return t;
  return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function applyTheme(t) {
  if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t);
  const btn = $('#theme-btn');
  const dark = effectiveTheme() === 'dark';
  // The label is a graph style's to flavour (graph/strings.js
  // `schemeToggle`), but only on the graph view: every other page is core.
  const say = makeT(onGraph ? currentStyle() : null);
  btn.textContent = say('schemeToggle', dark);
  btn.setAttribute('aria-label', say('schemeToggleAria', dark));
}

function toggleTheme() {
  const next = effectiveTheme() === 'dark' ? 'light' : 'dark';
  try {
    localStorage.setItem(THEME_KEY, next);
  } catch (_) {
    /* private mode: the toggle still works for this page */
  }
  applyTheme(next);
  // Charts read their width at render time; colours follow the CSS tokens.
}

export function parseRoute() {
  return parseHash(location.hash);
}

export { href };

/**
 * Fetch the project catalog again (on failure keep the copy we have): the
 * one fetched at boot goes stale when a drive is mounted or a project is
 * registered while the page is open.
 */
async function refreshCatalog() {
  try {
    state.catalog = await api.projects();
  } catch (_) {
    /* keep the old copy; the view says what it can */
  }
}

/** Set by boot's own fetch, so the first route does not fetch it again. */
let catalogFresh = false;

function project(id) {
  return state.catalog && state.catalog.projects.find((p) => p.id === id);
}

/** The project to show when the route names none: current, else the first readable one. */
function defaultProject() {
  const ps = (state.catalog && state.catalog.projects) || [];
  if (state.catalog && state.catalog.current && project(state.catalog.current)) return state.catalog.current;
  const ok = ps.find((p) => p.available && p.kind === 'project') || ps.find((p) => p.available);
  return ok ? ok.id : ps.length ? ps[0].id : null;
}

function renderSwitcher() {
  const sel = $('#proj-select');
  const groups = [
    ['Projects', (p) => p.kind === 'project'],
    ['Groups', (p) => p.kind === 'group'],
    ['Global', (p) => p.kind === 'global'],
  ];
  const opts = [];
  for (const [labelText, pred] of groups) {
    const ps = state.catalog.projects.filter(pred);
    if (!ps.length) continue;
    opts.push(
      h(
        'optgroup',
        { label: labelText },
        ps.map((p) =>
          h(
            'option',
            { value: p.id, selected: p.id === state.project, disabled: !p.available },
            (p.parent_id ? '↳ ' : '') + p.name + (p.current ? ' (here)' : '') + (p.available ? '' : ' — unavailable'),
          ),
        ),
      ),
    );
  }
  mount(sel, opts);
  sel.value = state.project || '';
}

function renderNav(route) {
  const id = state.project;
  for (const a of $$('#nav a')) {
    const v = a.getAttribute('data-v');
    a.setAttribute('href', v === 'projects' ? href('projects') : href(v, id));
    const on = v === route.view || (v === 'memories' && route.view === 'detail');
    if (on) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
}

let renderSeq = 0;
let firstRender = true;
let keepFocus = false;

async function route() {
  const seq = ++renderSeq;
  const r = parseRoute();
  if (r.view !== 'projects' && r.view !== 'notfound') {
    const id = r.id || defaultProject();
    if (!r.id && id) {
      // Canonicalize so the address bar names the project being shown.
      history.replaceState(null, '', href(r.view === 'overview' ? 'overview' : r.view, id));
    }
    state.project = id;
    r.id = id;
  }
  if (r.view !== 'notfound' && needsCatalog(r, state.catalog) && !catalogFresh) {
    await refreshCatalog();
    if (seq !== renderSeq) return;
  }
  catalogFresh = false;
  // A graph link's `style=` holds only while that graph is shown (the graph
  // view applies it); every other view uses this browser's stored pick.
  onGraph = r.view === 'graph';
  if (!onGraph) useStyle(null);
  applyTheme(null);
  renderSwitcher();
  renderNav(r);
  const main = $('#main');
  // `refresh` re-renders the current view in place (after a review write),
  // keeping the scroll position.
  const ctx = {
    state,
    route: r,
    href,
    project: project(r.id),
    isStale: () => seq !== renderSeq,
    // Go to `hash`; with keepFocus the new view leaves focus where the
    // caller puts it rather than moving it to the heading.
    navigate: (hash, opts) => {
      if (location.hash === hash) return;
      if (opts && opts.keepFocus) keepFocus = true;
      location.hash = hash;
    },
    // `{ scroll: false }`: the view places the scroll itself.
    // `{ focusId }`: once rendered, focus that element (a section heading),
    // since the refresh replaced whatever held focus.
    refresh: (opts) => {
      const y = window.scrollY;
      keepFocus = true;
      route().then(() => {
        if (!(opts && opts.scroll === false)) window.scrollTo(0, y);
        const el = opts && opts.focusId ? document.getElementById(opts.focusId) : null;
        if (el) {
          if (!el.hasAttribute('tabindex')) el.setAttribute('tabindex', '-1');
          el.focus({ preventScroll: true });
        }
      });
    },
  };
  // A refresh `ms` from now, unless the user has moved on to another view
  // meanwhile (it would render that view a second time).
  ctx.refreshSoon = (ms, opts) => setTimeout(() => !ctx.isStale() && ctx.refresh(opts), ms);
  try {
    if (r.view === 'notfound') mount(main, errorBox({ status: 404, message: 'This link is not valid.' }));
    else if (r.view === 'projects') await renderProjects(main, ctx);
    else if (!r.id) mount(main, h('p', { class: 'muted' }, 'No EngramDB projects are registered on this machine yet. Run `engramdb init` in a project.'));
    else if (!ctx.project) mount(main, errorBox({ status: 404, message: `No project ${r.id} on this machine.` }));
    else if (r.view === 'memories') await renderMemories(main, ctx);
    else if (r.view === 'detail') await renderDetail(main, ctx);
    else if (r.view === 'graph') await renderGraph(main, ctx);
    else if (r.view === 'relations') await renderRelations(main, ctx);
    else if (r.view === 'doctor') await renderDoctor(main, ctx);
    else await renderOverview(main, ctx);
  } catch (e) {
    if (seq === renderSeq) mount(main, errorBox(e));
  }
  // Move focus to the new view's heading on navigation (not on the first
  // load), so keyboard and screen-reader users land where the page changed.
  if (seq === renderSeq && !firstRender && !keepFocus) {
    const title = document.getElementById('view-title') || main;
    if (!title.hasAttribute('tabindex')) title.setAttribute('tabindex', '-1');
    title.focus({ preventScroll: false });
  }
  firstRender = false;
  // Only the newest render consumes the flag: a stale one finishing late
  // must not clear it under the render it was set for.
  if (seq === renderSeq) keepFocus = false;
}

async function boot() {
  takeToken();
  onStyleChange(() => applyTheme(null));
  useStyle(null);
  applyTheme(storedTheme());
  $('#theme-btn').addEventListener('click', toggleTheme);
  $('#proj-select').addEventListener('change', (ev) => {
    const r = parseRoute();
    const keep = r.view === 'detail' || r.view === 'projects' || r.view === 'notfound' ? 'overview' : r.view;
    location.hash = href(keep, ev.target.value);
  });
  try {
    [state.session, state.catalog] = await Promise.all([api.session(), api.projects()]);
  } catch (e) {
    mount($('#main'), errorBox(e));
    return;
  }
  const s = state.session;
  setCsrf(s.csrf);
  $('#brand-sub').textContent = `Memory dashboard · ${location.host}`;
  const tag = $('#mode-tag');
  if (s.allow_writes) {
    tag.textContent = 'Relation review on';
    tag.classList.add('warn');
    $('#foot-mode').textContent = 'Started with --allow-writes · confirming or rejecting a proposed relation writes to the store';
  }
  $('#foot-version').textContent = `engramdb ${s.version} · schema ${s.schema_version} · served by engramdb dashboard`;
  catalogFresh = true;
  window.addEventListener('hashchange', route);
  await route();
}

boot();
