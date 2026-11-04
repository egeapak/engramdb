// The graph legend's structure and words (core); every swatch is drawn by
// the style, so a legend always shows exactly what that style draws.

import { h, s } from '../dom.js';
import { fmt } from '../format.js';
import { badge, scopeName, TYPE_LABEL } from '../scopes.js';
import { KINDS, kindLabel } from '../relations.js';

/**
 * Three legend sections: scopes, memories by type, relations. The style's
 * samplers:
 *   scopeSample(scope)        what follows a scope's badge (or null)
 *   nodeSample(kind, type)    a legend memory (`kind`: hub, never, chal or null)
 *   edgeSample(kind, status)  a relation
 *   stubSample(state)         a relation ending at a `dangling` / `unknown` far end
 */
export function buildLegend(g, t, { scopeSample, nodeSample, edgeSample, stubSample }) {
  const types = [...new Set(g.nodes.map((n) => n.type))].sort();
  return [
    h(
      'div',
      null,
      h('h4', null, t('legendScopes')),
      h(
        'ul',
        null,
        g.scopes.slice(0, 12).map((l) => h('li', null, badge(l.name, l.slot), scopeSample(l), `${scopeName(l.name)} · ${fmt(l.count)}`)),
        g.scopes.length > 12 ? h('li', { class: 'faint' }, t('legendMoreScopes', g.scopes.length - 12)) : null,
      ),
    ),
    h(
      'div',
      null,
      h('h4', null, t('legendMemories')),
      h(
        'ul',
        null,
        types.map((ty) => h('li', null, nodeSample(null, ty), TYPE_LABEL[ty] || ty)),
        h('li', null, nodeSample('hub'), t('legendHub')),
        h('li', null, nodeSample('never'), t('legendNever')),
        h('li', null, nodeSample('chal'), t('legendChallenged')),
        h('li', null, s('svg', { width: 34, height: 16, viewBox: '0 0 34 16', 'aria-hidden': 'true', class: 'lg-size' }, s('circle', { cx: 6, cy: 8, r: 3.5 }), s('circle', { cx: 23, cy: 8, r: 7 })), t('legendSize')),
      ),
    ),
    h(
      'div',
      null,
      h('h4', null, t('legendRelations')),
      h(
        'ul',
        null,
        KINDS.map((k) => h('li', null, edgeSample(k, 'confirmed'), kindLabel(k))),
        h('li', null, edgeSample('contradicts', 'proposed'), t('legendProposedContradiction')),
        h('li', null, edgeSample('supports', 'proposed'), t('legendProposedOther')),
        h('li', null, edgeSample('related_to', 'rejected'), t('legendRejected')),
        h('li', null, stubSample('dangling'), t('legendDeleted')),
        h('li', null, stubSample('unknown'), t('legendUnknown')),
      ),
    ),
  ];
}
