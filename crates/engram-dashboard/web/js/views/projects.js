// Every store on the machine: projects (with worktrees and linked
// sub-projects under their parent), groups and the global store.

import { h, mount } from '../dom.js';
import { fmt, ago, shortId, plural } from '../format.js';
import { projHead, secHead } from './common.js';

function card(p, ctx) {
  const selected = p.id === ctx.state.project;
  const tags = [];
  if (p.current) tags.push(h('span', { class: 'kind-tag here' }, 'here'));
  if (p.worktree) tags.push(h('span', { class: 'kind-tag' }, 'worktree'));
  else if (p.parent_id) tags.push(h('span', { class: 'kind-tag' }, 'sub-project'));
  if (p.kind !== 'project') tags.push(h('span', { class: 'kind-tag' }, p.kind));
  // Navigation is a link: middle-click, copy-link and the browser's own
  // focus order all work. A sub-project's memories live in its parent.
  const target = p.available ? p.id : p.parent_id || null;
  const attrs = {
    class: ['card', selected && 'cur', !p.available && 'off'],
    'aria-current': selected ? 'true' : null,
    title: !p.available && p.parent_id ? 'Open the parent project, which holds these memories' : null,
  };
  return h(
    'li',
    { class: p.parent_id ? 'child' : null },
    h(
      target ? 'a' : 'div',
      target ? { ...attrs, href: ctx.href('overview', target) } : attrs,
      h('div', { class: 'pn' }, p.name, tags),
      h('div', { class: 'pp mono' }, p.path),
      p.available
        ? h(
            'div',
            { class: 'pm' },
            h('span', null, h('b', null, p.memory_count === undefined ? '—' : fmt(p.memory_count)), 'memories'),
            p.children && p.children.length ? h('span', null, h('b', null, fmt(p.children.length)), p.children.length === 1 ? 'sub-project' : 'sub-projects') : null,
            p.subscribers && p.subscribers.length ? h('span', null, h('b', null, fmt(p.subscribers.length)), 'subscribers') : null,
          )
        : h('div', { class: p.kind === 'project' ? 'why' : ['why', 'empty'] }, p.unavailable_reason || 'unavailable'),
      h('div', { class: 'pid mono' }, `${shortId(p.id)}${p.last_write_at ? ' · last write ' + ago(p.last_write_at) : ''}${p.schema && p.schema.needs_migration ? ' · schema ' + (p.schema.stored || '?') + ' (behind)' : ''}`),
    ),
  );
}

export function projectCards(list, ctx) {
  if (!list.length) return h('p', { class: 'muted' }, 'None.');
  return h('ul', { class: 'projects' }, list.map((p) => card(p, ctx)));
}

export async function renderProjects(main, ctx) {
  const all = ctx.state.catalog.projects;
  const projects = all.filter((p) => p.kind === 'project');
  const groups = all.filter((p) => p.kind === 'group');
  const global = all.filter((p) => p.kind === 'global');
  const here = all.find((p) => p.current);
  mount(
    main,
    projHead('Stores', 'Projects on this machine', [
      `${plural(projects.length, 'project')} · ${plural(groups.length, 'group')} · registry.json`,
      here ? `Started in ${here.name}` : 'Started outside any project',
    ]),
    secHead('Projects', h('span', { class: 'faint' }, 'Worktrees and linked sub-projects sit under their parent')),
    projectCards(projects, ctx),
    secHead('Groups', h('span', { class: 'faint' }, 'Shared by the projects subscribed to them')),
    projectCards(groups, ctx),
    secHead('Global', h('span', { class: 'faint' }, 'Read by every project')),
    projectCards(global, ctx),
  );
}
