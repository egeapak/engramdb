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
  const open = () => {
    if (!p.available && p.parent_id) location.hash = ctx.href('overview', p.parent_id);
    else if (p.available) location.hash = ctx.href('overview', p.id);
  };
  const disabled = !p.available && !p.parent_id;
  return h(
    'li',
    { class: p.parent_id ? 'child' : null },
    h(
      'button',
      {
        type: 'button',
        'aria-pressed': String(selected),
        'aria-disabled': disabled ? 'true' : null,
        onclick: disabled ? null : open,
        title: !p.available && p.parent_id ? 'Open the parent project, which holds these memories' : null,
      },
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
        : h('div', { class: p.kind === 'global' ? ['why', 'empty'] : 'why' }, p.unavailable_reason || 'unavailable'),
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
