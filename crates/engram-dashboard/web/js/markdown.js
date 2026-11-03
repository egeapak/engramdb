// A deliberately small Markdown subset, rendered to DOM nodes.
//
// Memory bodies are untrusted, so nothing here produces HTML text: every
// construct becomes createElement + text nodes, and anything outside the
// subset stays literal text. Supported: paragraphs, ATX headings (shown one
// level down so a memory cannot out-shout the page), - / * / 1. lists,
// > quotes, ``` fences, `code`, **bold**, *em* / _em_, and [text](url) for
// http(s) URLs only (other schemes, javascript: included, stay text).

import { h } from './dom.js';

function inline(text) {
  const out = [];
  // Order matters: code spans first, so their content is never re-parsed.
  const re = /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)|\*\*([^*]+)\*\*|\*([^*\s][^*]*)\*|_([^_\s][^_]*)_|\[([^\]]+)\]\(([^)\s]+)\)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1]) out.push(h('code', null, m[2]));
    else if (m[3] !== undefined) out.push(h('strong', null, ...inline(m[3])));
    else if (m[4] !== undefined) out.push(h('em', null, ...inline(m[4])));
    else if (m[5] !== undefined) out.push(h('em', null, ...inline(m[5])));
    else if (m[6] !== undefined) {
      const url = safeUrl(m[7]);
      out.push(url ? h('a', { href: url, rel: 'noopener noreferrer', target: '_blank' }, m[6]) : m[0]);
    }
    last = re.lastIndex;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function safeUrl(raw) {
  try {
    const u = new URL(raw);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null;
  } catch (_) {
    return null;
  }
}

/** Render `src` into an array of block nodes. */
export function renderMarkdown(src) {
  const lines = String(src || '').replace(/\r\n?/g, '\n').split('\n');
  const blocks = [];
  let para = [];
  let list = null;
  const flushPara = () => {
    if (para.length) blocks.push(h('p', null, ...inline(para.join(' '))));
    para = [];
  };
  const flushList = () => {
    if (list) blocks.push(list.el);
    list = null;
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^\s*```/);
    if (fence) {
      flushPara();
      flushList();
      const body = [];
      i += 1;
      while (i < lines.length && !/^\s*```/.test(lines[i])) body.push(lines[i++]);
      blocks.push(h('pre', null, h('code', null, body.join('\n'))));
      continue;
    }
    if (!line.trim()) {
      flushPara();
      flushList();
      continue;
    }
    const head = line.match(/^(#{1,6})\s+(.*)$/);
    if (head) {
      flushPara();
      flushList();
      const level = Math.min(6, head[1].length + 2);
      blocks.push(h('h' + level, null, ...inline(head[2])));
      continue;
    }
    const quote = line.match(/^>\s?(.*)$/);
    if (quote) {
      flushPara();
      flushList();
      blocks.push(h('blockquote', null, ...inline(quote[1])));
      continue;
    }
    const item = line.match(/^\s*(?:([-*+])|(\d+)[.)])\s+(.*)$/);
    if (item) {
      flushPara();
      const tag = item[1] ? 'ul' : 'ol';
      if (!list || list.tag !== tag) {
        flushList();
        list = { tag, el: h(tag) };
      }
      list.el.appendChild(h('li', null, ...inline(item[3])));
      continue;
    }
    if (list && /^\s{2,}\S/.test(line)) {
      // A continuation line of the last list item.
      const li = list.el.lastChild;
      li.appendChild(document.createTextNode(' '));
      for (const n of inline(line.trim())) li.append(n);
      continue;
    }
    flushList();
    para.push(line.trim());
  }
  flushPara();
  flushList();
  return blocks;
}
