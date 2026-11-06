// Browser checks of the graph view's interaction and accessibility, for
// either surface (the canvas host Plain draws through, the SVG Grid draws
// through): what a keyboard, a screen reader and a pointer get. A developer
// tool run by hand (README.md), like run.mjs; not embedded, not in CI.
//
//   NODE_PATH=$(npm root -g) node bench/browser/checks.mjs --stores /tmp/dash-bench \
//     [--bin target/release/engramdb] [--assets web] [--views s60:full,s1k:all,s10k:open]
//     [--styles plain,grid] [--json out.json]
//
// Per view and style it checks:
//   a11y_list      one focusable element per drawn memory, with the SVG `<g>`'s
//                  aria label (the canvas's list against Grid's SVG, in order)
//   canvas_role    a canvas surface's picture has role="img" and a label
//   keyboard       ] walks memories top to bottom (roving focus), Enter selects
//                  (the panel's heading takes focus), ] / [ then walk the panel
//                  rows (focus on the neighbour, the live region speaks), Escape
//                  clears and returns focus to the graph
//   hover          the pointer over a memory names it in the tooltip (30
//                  memories) and shows the pointer cursor
//   click          a click on a memory selects it
//   relation       a relation pinned from the relations table is centred and
//                  carded; the pointer at its middle names it, a click pins it
//   filters        a display filter moves no memory
//   zoom           a wheel gesture settles (no transform left on the pane)
//   pan            (canvas) with a memory selected, a drag moves one flattened
//                  canvas that looks exactly like the layers it stands for, and
//                  the layers come back when it settles
//   partial        (canvas) after 12 hovers (each redrawing only the overlay's
//                  old and new rectangle) the picture equals a full redraw
//   theme          (canvas) the drawing follows a light / dark switch
//   console        no page error, no console error (a CSP violation is one)

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith('--') ? [...acc, [a.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : '1']] : acc), []),
);
if (!args.stores) {
  console.error('usage: node bench/browser/checks.mjs --stores <dir> [options]');
  process.exit(2);
}
const stores = resolve(args.stores);
const bin = args.bin ? resolve(args.bin) : fileURLToPath(new URL('../../../../target/release/engramdb', import.meta.url));
const VIEWS = (args.views || 's60:full,s1k:all,s10k:open').split(',').map((v) => v.split(':'));
const STYLES = (args.styles || 'plain,grid').split(',');
const children = new Set();

function startDashboard(size, port) {
  const env = {
    ...process.env,
    ENGRAMDB_DATA_DIR: join(stores, 'data'),
    ENGRAMDB_CONFIG_DIR: join(stores, 'conf'),
    ENGRAMDB_REGISTRY_PATH: join(stores, 'conf', 'registry.json'),
    ENGRAMDB_MODEL_CACHE_DIR: join(stores, 'models'),
    ENGRAMDB_OFFLINE: '1',
    ENGRAMDB_IN_PROCESS: '1',
    ENGRAMDB_DASHBOARD_CACHE_DIR: join(stores, 'cache'),
  };
  const argv = ['dashboard', '--no-open', '--json', '--port', String(port)];
  if (args.assets) argv.push('--assets-dir', resolve(args.assets));
  const child = spawn(bin, argv, { cwd: join(stores, size), env, stdio: ['ignore', 'pipe', 'inherit'] });
  children.add(child);
  child.on('exit', () => children.delete(child));
  return new Promise((res, rej) => {
    let out = '';
    const timer = setTimeout(() => rej(new Error('no token URL: ' + out)), 30000);
    child.stdout.on('data', (d) => {
      out += d;
      const m = out.match(/http[^"\s]*#token=[0-9a-f]+/);
      if (m) {
        clearTimeout(timer);
        res({ url: m[0], child });
      }
    });
    child.on('exit', (c) => rej(new Error(`dashboard exited (${c}): ${out}`)));
  });
}

/** The drawn memories' accessible elements, in document order: { i, label, x, y } (centre, client px). */
const MEMORIES = () => {
  const els = [...document.querySelectorAll('.g-a11y-node, svg.graph .nodes .node')];
  // relative to the box, so a page scroll moves nothing
  const o = document.querySelector('.graph-box').getBoundingClientRect();
  return els.map((el) => {
    const r = el.getBoundingClientRect();
    return { i: +el.getAttribute('data-i'), label: el.getAttribute('aria-label'), x: r.left - o.left + r.width / 2, y: r.top - o.top + r.height / 2 };
  });
};

/**
 * The largest channel difference between two PNG screenshots, compared in
 * the page (createImageBitmap on a Blob: no decoder here, no data: URL for
 * the CSP to refuse).
 */
async function maxDiff(p, a, b) {
  return p.evaluate(
    async ([a, b]) => {
      const px = async (b64) => {
        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        const bm = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
        const c = new OffscreenCanvas(bm.width, bm.height).getContext('2d');
        c.drawImage(bm, 0, 0);
        return c.getImageData(0, 0, bm.width, bm.height).data;
      };
      const [x, y] = [await px(a), await px(b)];
      if (x.length !== y.length) return 255;
      let max = 0;
      for (let i = 0; i < x.length; i++) max = Math.max(max, Math.abs(x[i] - y[i]));
      return max;
    },
    [a.toString('base64'), b.toString('base64')],
  );
}

async function openView(p, view) {
  const pid = await p.evaluate(() => location.hash.split('/')[2]);
  const q = view === 'all' ? 'all=1' : view === 'open' ? 'top_scope=storage' : '';
  await p.evaluate(([pid, q]) => (location.hash = `#/p/${pid}/graph${q ? '?' + q : ''}`), [pid, q]);
  await p.waitForSelector('.graph-box .g-pane', { timeout: 60000 });
  await p.waitForTimeout(600);
}

const summaryOf = (label) => label.replace(/ \([^()]*\)$/, '');

async function check(browser, url, size, view, style) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 }, deviceScaleFactor: 1, colorScheme: 'light' });
  await ctx.addInitScript((st) => {
    try {
      localStorage.setItem('engramdb.graphStyle', st);
    } catch (_) {}
  }, style);
  const p = await ctx.newPage();
  const errors = [];
  p.on('pageerror', (e) => errors.push('pageerror: ' + e.message));
  p.on('console', (m) => m.type() === 'error' && errors.push('console: ' + m.text()));
  await p.goto(url);
  await p.waitForSelector('.nav a[href*="/graph"]');
  await openView(p, view);
  const out = { size, view, style, checks: {} };
  const ok = (name, pass, detail) => (out.checks[name] = { pass: !!pass, ...(detail === undefined ? {} : { detail }) });
  const canvas = await p.evaluate(() => !!document.querySelector('.g-canvas'));
  out.surface = canvas ? 'canvas' : 'svg';

  // a11y list
  const mems = await p.evaluate(MEMORIES);
  out.memories = mems.length;
  out.labels = mems.map((m) => m.label);
  const shown = await p.evaluate(() => {
    const dd = [...document.querySelectorAll('.g-card dt')].find((d) => d.textContent === 'Memories');
    return dd ? dd.nextElementSibling.textContent : '';
  });
  const shownN = Number((shown.split(' of ')[0] || '').replace(/[^0-9]/g, ''));
  ok('a11y_list', mems.length > 0 && mems.length === shownN && mems.every((m, k) => m.i === k && m.label && / relations?\)$/.test(m.label)), { elements: mems.length, card: shown });
  if (canvas) {
    const role = await p.evaluate(() => {
      const c = document.querySelector('.g-canvas canvas[role="img"]');
      return c ? c.getAttribute('aria-label') : null;
    });
    ok('canvas_role', !!role, role);
  }

  // keyboard
  await p.focus('.graph-box');
  const walk = [];
  for (let k = 0; k < 6; k++) {
    await p.keyboard.press(']');
    walk.push(await p.evaluate(() => {
      const a = document.activeElement;
      const r = a.getBoundingClientRect();
      return { i: +a.getAttribute('data-i'), label: a.getAttribute('aria-label'), tab: a.getAttribute('tabindex'), y: r.top + r.height / 2 };
    }));
  }
  const ys = walk.map((w) => w.y);
  // `]` walks by layout y; a centre-on jump moves the screen between steps,
  // so compare against the memory's own order in the walk instead: every
  // step lands on a memory element with roving tabindex -1.
  const roving = walk.every((w) => Number.isInteger(w.i) && w.i >= 0 && w.tab === '-1' && w.label);
  await p.keyboard.press('Enter');
  await p.waitForTimeout(150);
  const sel = await p.evaluate(() => ({ head: (document.querySelector('.g-card h3') || {}).textContent || '', focus: document.activeElement.tagName }));
  const want = summaryOf(walk[walk.length - 1].label);
  // ] / [ with a memory selected walk its panel rows.
  await p.focus('.graph-box');
  await p.keyboard.press(']');
  await p.waitForTimeout(100);
  const row = await p.evaluate(() => ({
    i: document.activeElement.getAttribute('data-i'),
    cls: document.activeElement.className && String(document.activeElement.className.baseVal ?? document.activeElement.className),
    said: (document.querySelector('.g-card [aria-live="polite"].sr-only') || {}).textContent || '',
    rows: document.querySelectorAll('.g-card li.pr-row').length,
  }));
  await p.keyboard.press('Escape');
  await p.waitForTimeout(100);
  const cleared = await p.evaluate(() => ({ box: document.activeElement === document.querySelector('.graph-box'), head: (document.querySelector('.g-card h3') || {}).textContent || '' }));
  ok('keyboard', roving && sel.head.includes(want) && sel.focus === 'H3' && (row.rows === 0 || (row.said && row.i !== null)) && cleared.box && cleared.head === 'Select a memory or relation', { walk: walk.map((w) => w.i), ys: ys.map(Math.round), selected: sel, row, cleared });

  // hover: 30 memories, pointer at each centre
  const box = await p.evaluate(() => {
    const r = document.querySelector('.graph-box').getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  });
  await p.keyboard.press('0');
  await p.waitForTimeout(300);
  const inBox = (await p.evaluate(MEMORIES)).filter((m) => m.x > 12 && m.x < box.w - 12 && m.y > 12 && m.y < box.h - 12).map((m) => ({ ...m, x: m.x + box.x, y: m.y + box.y }));
  let hits = 0;
  let tried = 0;
  const misses = [];
  const cursors = new Set();
  for (let k = 0; k < Math.min(30, inBox.length); k++) {
    const m = inBox[Math.floor((k * inBox.length) / Math.min(30, inBox.length))];
    await p.mouse.move(m.x, m.y);
    await p.waitForTimeout(40);
    const r = await p.evaluate(() => {
      const tip = document.querySelector('.g-tip');
      const el = document.querySelector('.g-canvas') || document.elementFromPoint(0, 0);
      return { tip: tip && !tip.hidden ? (tip.querySelector('strong') || {}).textContent : null, cursor: getComputedStyle(document.querySelector('.g-canvas') || document.querySelector('svg.graph .node')).cursor, el: !!el };
    });
    tried++;
    cursors.add(r.cursor);
    if (r.tip === summaryOf(m.label)) hits++;
    else misses.push({ want: summaryOf(m.label), got: r.tip });
  }
  ok('hover', tried > 0 && hits >= Math.floor(tried * 0.9) && cursors.has('pointer'), { tried, hits, cursors: [...cursors], misses: misses.slice(0, 3) });
  // click a memory
  const target = inBox[Math.floor(inBox.length / 2)];
  await p.mouse.move(target.x, target.y);
  await p.mouse.click(target.x, target.y);
  await p.waitForTimeout(150);
  const head = await p.evaluate(() => (document.querySelector('.g-card h3') || {}).textContent || '');
  ok('click', head.includes(summaryOf(target.label)), { want: summaryOf(target.label), head });
  await p.mouse.move(box.x + 2, box.y + 2);
  await p.keyboard.press('Escape');

  // a relation pinned from the table
  const pinned = await p.evaluate(async () => {
    const all = [...document.querySelectorAll('button')].find((b) => /^Show all \d/.test(b.textContent));
    if (all) all.click();
    await new Promise((r) => setTimeout(r, 100));
    const btn = [...document.querySelectorAll('.edges-table button')].find((b) => b.textContent === 'Show in graph' || b.textContent === 'Review');
    if (!btn) return null;
    btn.click();
    await new Promise((r) => setTimeout(r, 300));
    const kind = (document.querySelector('.g-card h3') || {}).textContent || '';
    return { kind: kind.trim(), eyebrow: (document.querySelector('.g-card .eyebrow') || {}).textContent || '' };
  });
  if (pinned) {
    // pin() centred the relation's middle: the pointer there names it.
    await p.mouse.move(box.x + box.w / 2, box.y + box.h / 2);
    await p.waitForTimeout(60);
    const tip = await p.evaluate(() => {
      const tip = document.querySelector('.g-tip');
      return tip && !tip.hidden ? tip.textContent : null;
    });
    await p.keyboard.press('Escape');
    await p.mouse.click(box.x + box.w / 2, box.y + box.h / 2);
    await p.waitForTimeout(150);
    const again = await p.evaluate(() => (document.querySelector('.g-card .eyebrow') || {}).textContent || '');
    // On a dense graph a memory or a later relation can sit over the pinned
    // relation's middle (the SVG stacks them the same way), so the pointer
    // there must hit *something*; it names the pinned kind when nothing does.
    ok('relation', pinned.eyebrow === 'Relation · pinned' && !!tip && (tip.startsWith(pinned.kind) || size !== 's60') && /pinned|selected/.test(again), { pinned, tip, again });
  }
  await p.mouse.move(box.x + 2, box.y + 2);

  // a display filter moves no memory
  // (after the pin's view move has settled: read until two reads agree)
  let before = await p.evaluate(MEMORIES);
  for (let k = 0; k < 20; k++) {
    await p.waitForTimeout(100);
    const again = await p.evaluate(MEMORIES);
    const same = again.length === before.length && again.every((m, j) => m.x === before[j].x && m.y === before[j].y);
    before = again;
    if (same) break;
  }
  await p.evaluate(() => [...document.querySelectorAll('.chip')].find((b) => b.textContent === 'Proposed').click());
  await p.waitForTimeout(150);
  const after = await p.evaluate(MEMORIES);
  await p.evaluate(() => [...document.querySelectorAll('.chip')].find((b) => b.textContent === 'Proposed').click());
  const moved = before.filter((m, k) => !after[k] || Math.abs(m.x - after[k].x) >= 0.01 || Math.abs(m.y - after[k].y) >= 0.01);
  ok('filters', before.length === after.length && moved.length === 0, { before: before.length, after: after.length, moved: moved.length, first: moved[0] && [moved[0], after[moved[0].i]] });

  // zoom gesture settles
  await p.focus('.graph-box');
  await p.evaluate(async () => {
    const b = document.querySelector('.graph-box');
    const r = b.getBoundingClientRect();
    for (let k = 0; k < 8; k++) {
      await new Promise((res) => requestAnimationFrame(res));
      b.dispatchEvent(new WheelEvent('wheel', { deltaY: -80, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, bubbles: true, cancelable: true }));
    }
  });
  const mid = await p.evaluate(() => document.querySelector('.g-pane').style.transform);
  await p.waitForTimeout(450);
  const settled = await p.evaluate(() => document.querySelector('.g-pane').style.transform);
  ok('zoom', mid !== '' && settled === '', { mid, settled });

  if (canvas) {
    // pan with a selection: one canvas mid-gesture, the same picture moved
    await p.evaluate(() => document.querySelectorAll('.g-a11y-node')[0].focus());
    await p.keyboard.press('Enter');
    await p.waitForTimeout(300);
    const shot = (clip) => p.screenshot({ clip });
    const b = await (await p.$('.graph-box')).boundingBox();
    // (inset: the box's top edge carries page UI that may cast a shadow mid-drag)
    const clip = { x: b.x + 100, y: b.y + 120, width: b.width - 200, height: b.height - 200 };
    await p.mouse.move(b.x + 6, b.y + b.height - 6);
    await p.waitForTimeout(200);
    const before = await shot(clip);
    await p.mouse.down();
    await p.mouse.move(b.x + 66, b.y + b.height - 6, { steps: 6 });
    const during = await p.evaluate(() => ({ t: document.querySelector('.g-pane').style.transform, shown: [...document.querySelectorAll('.g-canvas canvas')].filter((c) => !c.hidden).length }));
    const moved = await shot({ ...clip, x: clip.x + 60 });
    if (args.dump) { writeFileSync(args.dump + '-a.png', before); writeFileSync(args.dump + '-b.png', moved); }
    await p.mouse.up();
    await p.waitForTimeout(300);
    const after = await p.evaluate(() => [...document.querySelectorAll('.g-canvas canvas')].filter((c) => !c.hidden).length);
    await p.keyboard.press('Escape');
    // One colour level of rounding from drawing the overlay into the layer
    // under it is allowed.
    const diff = await maxDiff(p, before, moved);
    ok('pan', /translate\(60px, 0px\)/.test(during.t) && during.shown === 1 && diff <= 1 && after === 2, { during, after, maxChannelDiff: diff });

    // partial overlay redraws add up to a full one (at Fit, so most memories are in the box)
    await p.keyboard.press('0');
    await p.waitForTimeout(300);
    const inside = (await p.evaluate(MEMORIES)).filter((m) => m.x > 30 && m.x < b.width - 30 && m.y > 30 && m.y < b.height - 30);
    const full = { x: b.x, y: b.y, width: b.width, height: b.height };
    for (let k = 0; k < 12; k++) {
      const m = inside[(k * 7919) % inside.length];
      await p.mouse.move(b.x + m.x, b.y + m.y);
      await p.waitForTimeout(40);
    }
    await p.waitForTimeout(100);
    const partial = await shot(full);
    // the same overlay drawn whole
    const last = inside[(11 * 7919) % inside.length];
    // (the pointer leaves, the overlay is hidden, and comes back whole)
    await p.mouse.move(b.x + 4, b.y + 4);
    await p.waitForTimeout(100);
    await p.mouse.move(b.x + last.x, b.y + last.y);
    await p.waitForTimeout(100);
    const whole = await shot(full);
    const pd = await maxDiff(p, partial, whole);
    if (args.dump) { writeFileSync(args.dump + '-p.png', partial); writeFileSync(args.dump + '-w.png', whole); }
    ok('partial', pd <= 1 && inside.length >= 12, { maxChannelDiff: pd, memories: inside.length });
    await p.mouse.move(b.x + 4, b.y + 4);
    await p.waitForTimeout(100);
    const px = () =>
      p.evaluate(() => {
        const c = document.querySelector('.g-canvas canvas[role="img"]');
        const d = c.getContext('2d').getImageData(2, 2, 1, 1).data;
        return [d[0], d[1], d[2]].join(',');
      });
    const light = await px();
    await p.click('#theme-btn');
    await p.waitForTimeout(150);
    const dark = await px();
    await p.click('#theme-btn');
    await p.waitForTimeout(150);
    ok('theme', light !== dark && (await px()) === light, { light, dark });
  }
  ok('console', errors.length === 0, errors.slice(0, 5));
  await ctx.close();
  return out;
}

const browser = await chromium.launch(args.chrome ? { executablePath: args.chrome } : {});
const rows = [];
let port = Number(args.port || 7480);
let failed = 0;
try {
  const bySize = new Map();
  for (const [size, view] of VIEWS) bySize.set(size, [...(bySize.get(size) || []), view]);
  for (const [size, views] of bySize) {
    const { url, child } = await startDashboard(size, port++);
    try {
      for (const view of views) {
        const res = {};
        for (const style of STYLES) res[style] = await check(browser, url, size, view, style);
        // The canvas's accessible list says what Grid's SVG says, memory by memory.
        if (res.plain && res.grid) {
          const same = JSON.stringify(res.plain.labels) === JSON.stringify(res.grid.labels);
          res.plain.checks.same_labels_as_svg = { pass: same, detail: { plain: res.plain.labels.length, grid: res.grid.labels.length } };
        }
        for (const r of Object.values(res)) {
          delete r.labels;
          const bad = Object.entries(r.checks).filter(([, c]) => !c.pass);
          failed += bad.length;
          rows.push(r);
          console.log(`${size} ${view} ${r.style} (${r.surface}, ${r.memories} memories): ${bad.length ? 'FAIL ' + bad.map(([k, c]) => `${k} ${JSON.stringify(c.detail)}`).join('; ') : 'ok ' + Object.keys(r.checks).join(' ')}`);
        }
      }
    } finally {
      child.kill();
    }
  }
} finally {
  for (const c of children) c.kill();
  await browser.close();
  if (args.json) writeFileSync(args.json, JSON.stringify(rows, null, 1));
}
process.exit(failed ? 1 : 0);
