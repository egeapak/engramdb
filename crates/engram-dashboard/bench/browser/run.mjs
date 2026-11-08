// The dashboard's browser bench: runs `engramdb dashboard --no-open` on each
// benchmark store and times the graph and relations views in Chromium,
// from the page's own performance marks and requestAnimationFrame. A
// developer tool (README.md); not embedded, not in CI.
//
//   NODE_PATH=$(npm root -g) node bench/browser/run.mjs --stores /tmp/dash-bench \
//     [--bin target/release/engramdb] [--sizes s60,s1k,s2k,s10k] [--styles plain,grid]
//     [--themes light,dark] [--widths 1280,390] [--runs 5] [--assets web] [--chrome PATH]
//     [--json out.json] [--cpu-throttle 4]
//
// Every figure is a median over `--runs` cold page loads (a fresh browser
// context, so no layout cache) unless it says otherwise.

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
const list = (k, d) => (args[k] || d).split(',').filter(Boolean);
const stores = resolve(args.stores || '');
if (!args.stores) {
  console.error('usage: node bench/browser/run.mjs --stores <dir from `cargo run --example dashboard_bench_store`> [options]');
  process.exit(2);
}
// Default: the workspace's release binary (this file is crates/engram-dashboard/bench/browser/).
const bin = args.bin ? resolve(args.bin) : fileURLToPath(new URL('../../../../target/release/engramdb', import.meta.url));
const sizes = list('sizes', 's60,s1k,s2k,s10k');
const styles = list('styles', 'plain,grid');
const themes = list('themes', 'light,dark');
const widths = list('widths', '1280,390').map(Number);
const runs = Number(args.runs || 5);
/** The largest top scope of every store (dashboard_store.rs: Zipf over TOPS). */
const OPEN = args.scope || 'storage';

const med = (a) => {
  const s = a.filter((x) => Number.isFinite(x)).sort((x, y) => x - y);
  return s.length ? s[s.length >> 1] : NaN;
};
const pct = (a, f) => {
  const s = a.slice().sort((x, y) => x - y);
  return s.length ? s[Math.min(s.length - 1, Math.floor(f * s.length))] : NaN;
};

/** Start the dashboard on one store; resolves to its token URL. */
/** Every dashboard this run started, killed on any exit (see the end). */
const children = new Set();

/** Start the dashboard on one store; resolves to its token URL (30 s at most). */
function startDashboard(size, port) {
  const env = {
    ...process.env,
    ENGRAMDB_DATA_DIR: join(stores, 'data'),
    ENGRAMDB_CONFIG_DIR: join(stores, 'conf'),
    // The bench machine's own registry, whatever the shell exports: never a real one.
    ENGRAMDB_REGISTRY_PATH: join(stores, 'conf', 'registry.json'),
    ENGRAMDB_MODEL_CACHE_DIR: join(stores, 'models'),
    ENGRAMDB_OFFLINE: '1',
    ENGRAMDB_IN_PROCESS: '1',
    // The server-side layout cache (src/layout/cache.rs) under the bench dir,
    // never the user's; `--no-layout-cache` turns it off (every layout cold).
    ENGRAMDB_DASHBOARD_CACHE_DIR: join(stores, 'cache'),
    ...(args['no-layout-cache'] ? { ENGRAMDB_DASHBOARD_LAYOUT_CACHE: '0' } : {}),
  };
  const argv = ['dashboard', '--no-open', '--json', '--port', String(port)];
  if (args.assets) argv.push('--assets-dir', resolve(args.assets));
  const child = spawn(bin, argv, { cwd: join(stores, size), env, stdio: ['ignore', 'pipe', 'inherit'] });
  children.add(child);
  child.on('exit', () => children.delete(child));
  return new Promise((res, rej) => {
    let out = '';
    const timer = setTimeout(() => {
      child.kill();
      rej(new Error(`dashboard printed no token URL within 30 s: ${out}`));
    }, 30000);
    child.on('error', (e) => {
      clearTimeout(timer);
      rej(e);
    });
    child.stdout.on('data', (d) => {
      out += d;
      const m = out.match(/http[^"\s]*#token=[0-9a-f]+/);
      if (m) {
        clearTimeout(timer);
        res({ url: m[0], child });
      }
    });
    child.on('exit', (c) => {
      clearTimeout(timer);
      rej(new Error(`dashboard exited (${c}): ${out}`));
    });
  });
}

// In the page: resolve when a named mark is set AND the frame after it has
// painted (rAF, then a task: the frame's paint has been handed off by then).
// Registered synchronously inside performance.mark, so no frame is missed.
function initScript() {
  const orig = performance.mark.bind(performance);
  const waiters = new Map();
  window.__afterPaint = () => new Promise((r) => requestAnimationFrame(() => {
    const ch = new MessageChannel();
    ch.port1.onmessage = () => r(performance.now());
    ch.port2.postMessage(0);
  }));
  performance.mark = (name, o) => {
    const r = orig(name, o);
    const w = waiters.get(name);
    if (w) {
      waiters.delete(name);
      for (const f of w) window.__afterPaint().then(f);
    }
    return r;
  };
  window.__painted = (...names) => new Promise((res) => {
    for (const n of names) waiters.set(n, [...(waiters.get(n) || []), (t) => res({ name: n, t })]);
  });
  // Every mark set since t0 (the latest of each name) and the layout's source.
  window.__marksSince = (t0) => {
    const out = {};
    for (const e of performance.getEntriesByType('mark')) {
      if (e.startTime < t0) continue;
      out[e.name] = e.startTime - t0;
      if ((e.name === 'graph:layout' || e.name === 'graph:overview-layout') && e.detail) out.source = e.detail.source;
    }
    return out;
  };
  window.__since = (name, t0) => {
    const e = performance.getEntriesByName(name).filter((x) => x.startTime >= t0).pop();
    return e ? e.startTime - t0 : NaN;
  };
}

/** Click `sel` and time it to the frame after one of `marks` (plus the marks on the way). */
async function clickToPaint(p, sel, marks, extra = []) {
  await p.waitForSelector(sel, { state: 'attached', timeout: 60000 });
  return p.evaluate(
    async ({ sel, marks, extra }) => {
      const done = window.__painted(...marks);
      await window.__afterPaint();
      const t0 = performance.now();
      document.querySelector(sel).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, view: window }));
      const { name, t } = await done;
      const out = { ms: t - t0, mark: name, marks: window.__marksSince(t0) };
      for (const m of extra) out[m] = window.__since(m, t0);
      return out;
    },
    { sel, marks, extra },
  );
}

async function newPage(browser, url, style, theme, width) {
  const ctx = await browser.newContext({
    viewport: width < 600 ? { width, height: 844 } : { width, height: 860 },
    deviceScaleFactor: 1,
    colorScheme: theme,
  });
  await ctx.addInitScript(initScript);
  await ctx.addInitScript((st) => {
    try {
      localStorage.setItem('engramdb.graphStyle', st);
    } catch (_) {}
  }, style);
  const p = await ctx.newPage();
  // `--cpu-throttle N`: Chrome's CPU slowdown, so a frame-capped figure
  // (zoom and pan fps sit at 60 on a fast machine) can show a difference.
  if (args['cpu-throttle']) await (await ctx.newCDPSession(p)).send('Emulation.setCPUThrottlingRate', { rate: Number(args['cpu-throttle']) });
  p.on('pageerror', (e) => console.error('pageerror', e.message));
  await p.goto(url);
  await p.waitForSelector('.nav a[href*="/graph"]');
  await p.waitForTimeout(300);
  return { ctx, p };
}

/** A display-filter toggle (the Proposed chip) to painted, ×10 (5 off, 5 on). */
async function filterToggle(p) {
  return p.evaluate(async () => {
    const ts = [];
    for (let i = 0; i < 10; i++) {
      const btn = [...document.querySelectorAll('.chip')].find((b) => b.textContent === 'Proposed');
      await window.__afterPaint();
      const t0 = performance.now();
      btn.click();
      ts.push((await window.__afterPaint()) - t0);
    }
    return ts;
  });
}

/**
 * Hover 60 memories: hover → painted, and every frame's length meanwhile.
 * Both surfaces: an SVG memory gets the mouseout / mouseover a pointer
 * moving between memories fires (the page listens on the layer); a canvas
 * gets the pointermove at the memory's centre, so its hit test is counted.
 * Only memories whose centre is inside the box, on either surface.
 */
async function hoverFrames(p) {
  return p.evaluate(async () => {
    const o = document.querySelector('.graph-box').getBoundingClientRect();
    const all = [...document.querySelectorAll('svg.graph .nodes .node, .g-a11y-node')].map((el) => {
      const r = el.getBoundingClientRect();
      return { el, x: r.left + r.width / 2, y: r.top + r.height / 2 };
    });
    const nodes = all.filter((n) => n.x > o.left + 8 && n.x < o.right - 8 && n.y > o.top + 8 && n.y < o.bottom - 8);
    const host = document.querySelector('.g-canvas');
    const gaps = [];
    let last = 0;
    let run = true;
    const tick = (t) => {
      if (last) gaps.push(t - last);
      last = t;
      if (run) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    const hover = [];
    let prev = null;
    const move = (to, from) => {
      if (host) {
        const at = to || { x: o.left + 2, y: o.top + 2 };
        host.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, clientX: at.x, clientY: at.y, pointerType: 'mouse' }));
        return;
      }
      if (from) from.el.dispatchEvent(new MouseEvent('mouseout', { bubbles: true, relatedTarget: to && to.el }));
      if (to) to.el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true, relatedTarget: from && from.el }));
    };
    for (let i = 0; i < 60; i++) {
      const n = nodes[(i * 7919) % nodes.length];
      await window.__afterPaint();
      const t0 = performance.now();
      move(n, prev);
      hover.push((await window.__afterPaint()) - t0);
      prev = n;
      await new Promise((r) => setTimeout(r, 30));
    }
    run = false;
    // The hover's own frame: the events dispatched at the start of a frame
    // (in a rAF callback), to the task after that frame's paint. That is
    // the main-thread work the 16 ms budget is about, without the wait for
    // the next vsync that `hover` above includes.
    const work = [];
    for (let i = 0; i < 60; i++) {
      const n = nodes[(i * 104729 + 13) % nodes.length];
      await window.__afterPaint();
      work.push(
        await new Promise((res) =>
          requestAnimationFrame(() => {
            const t0 = performance.now();
            move(n, prev);
            const ch = new MessageChannel();
            ch.port1.onmessage = () => res(performance.now() - t0);
            ch.port2.postMessage(0);
          }),
        ),
      );
      prev = n;
      await new Promise((r) => setTimeout(r, 30));
    }
    move(null, prev);
    return { hover, work, gaps: gaps.slice(3) };
  });
}

/** Frame gaps while `gesture` runs in Node (real mouse input through CDP). */
async function framesDuring(p, gesture) {
  await p.evaluate(() => {
    window.__gaps = [];
    window.__run = true;
    let last = 0;
    const tick = (t) => {
      if (last) window.__gaps.push(t - last);
      last = t;
      if (window.__run) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  await gesture();
  return p.evaluate(() => {
    window.__run = false;
    return window.__gaps.slice(2);
  });
}

/** Zoom: one wheel event per frame, 40 in then 40 out, around the box centre. */
async function zoomFrames(p) {
  await p.focus('.graph-box');
  return p.evaluate(async () => {
    const box = document.querySelector('.graph-box');
    const r = box.getBoundingClientRect();
    const gaps = [];
    let last = 0;
    for (let i = 0; i < 80; i++) {
      const t = await new Promise((res) => requestAnimationFrame(res));
      if (last) gaps.push(t - last);
      last = t;
      box.dispatchEvent(new WheelEvent('wheel', { deltaY: i < 40 ? -60 : 60, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, bubbles: true, cancelable: true }));
    }
    await new Promise((res) => setTimeout(res, 400)); // let the settle redraw run
    return gaps.slice(1);
  });
}

async function panFrames(p) {
  const r = await (await p.$('.graph-box')).boundingBox();
  const cx = r.x + r.width / 2;
  const cy = r.y + r.height / 2;
  return framesDuring(p, async () => {
    await p.mouse.move(cx, cy);
    await p.mouse.down();
    for (let i = 0; i < 60; i++) await p.mouse.move(cx + 120 * Math.sin(i / 9), cy + 60 * Math.cos(i / 9));
    await p.mouse.up();
  });
}

/**
 * Where a paint's time went, from the page's marks: the graph view's build
 * steps in order (views/graph.js), or the overview's; `paint` is the last
 * mark to the frame after it (style, layout and paint).
 */
const GRAPH_SEGS = [
  ['response', 'graph:data'],
  ['layout', 'graph:layout'],
  ['backdrop', 'graph:backdrop'],
  ['nodes', 'graph:nodes'],
  ['mount', 'graph:mounted'],
  ['view', 'graph:view'],
  ['legend', 'graph:legend'],
  ['edges', 'graph:edges'],
  ['panels', 'graph:built'],
];
const OVERVIEW_SEGS = [
  ['response', 'graph:data'],
  ['layout', 'graph:overview-layout'],
  ['build', 'graph:overview-built'],
];
function segments(marks, total, overview) {
  const out = {};
  let prev = 0;
  for (const [k, name] of overview ? OVERVIEW_SEGS : GRAPH_SEGS) {
    const t = marks[name];
    if (!Number.isFinite(t)) continue;
    out[k] = t - prev;
    prev = t;
  }
  out.paint = total - prev;
  return out;
}
/** Per-key medians of segment objects, plus the layout sources seen. */
function medSegments(list) {
  const out = {};
  for (const k of new Set(list.flatMap((o) => Object.keys(o)))) out[k] = med(list.map((o) => o[k]));
  return out;
}
const sources = (list) => [...new Set(list.filter(Boolean))].join('+') || undefined;

const fps = (gaps) => (gaps.length ? 1000 / (gaps.reduce((a, b) => a + b, 0) / gaps.length) : NaN);

async function benchCombo(browser, url, size, style, theme, width) {
  const row = { size, style, theme, width };
  const tab = [], data = [], ovPaint = [], open = [], openData = [], openLayout = [], openBuild = [], all = [], allLayout = [], rel = [];
  const tabSeg = [], openSeg = [], allSeg = [], tabSrc = [], openSrc = [], allSrc = [];
  let interactive = null;
  for (let r = 0; r < runs; r++) {
    const { ctx, p } = await newPage(browser, url, style, theme, width);
    // Tab click → first full paint (the overview past 150 memories).
    const t = await clickToPaint(p, '.nav a[href*="/graph"]', ['graph:built', 'graph:overview-built'], ['graph:data', 'graph:layout']);
    tab.push(t.ms);
    data.push(t['graph:data']);
    tabSeg.push(segments(t.marks, t.ms, t.mark === 'graph:overview-built'));
    tabSrc.push(t.marks.source);
    row.first_view = t.mark === 'graph:overview-built' ? 'overview' : 'graph';
    if (t.mark === 'graph:overview-built') {
      ovPaint.push(t.ms);
      // Open the largest scope from the overview.
      const o = await clickToPaint(p, `.so-scope[aria-label^="${OPEN}:"]`, ['graph:built'], ['graph:data', 'graph:layout', 'graph:built']);
      open.push(o.ms);
      openSeg.push(segments(o.marks, o.ms, false));
      openSrc.push(o.marks.source);
      openData.push(o['graph:data']);
      openLayout.push(o['graph:layout'] - o['graph:data']);
      openBuild.push(o['graph:built'] - o['graph:layout']);
    }
    if (r === 0 && !args['paths-only']) interactive = p;
    else await ctx.close();
  }
  // The interactive measurements, once, on the view a user lands on: the
  // opened scope past 150 memories, else the whole graph.
  const p = interactive;
  if (p) {
  await p.waitForTimeout(500);
  row.surface = await p.evaluate(() => (document.querySelector('.g-canvas') ? 'canvas' : 'svg'));
  row.shown_nodes = await p.evaluate(() => document.querySelectorAll('svg.graph .nodes .node, .g-a11y-node').length);
  // A canvas has no element per relation: the card's count (display filters
  // applied) stands in.
  row.shown_relations = await p.evaluate(() => {
    if (!document.querySelector('.g-canvas')) return document.querySelectorAll('svg.graph .edges > g:not([hidden])').length;
    const dt = [...document.querySelectorAll('.g-card dt')].find((d) => d.textContent === 'Relations');
    return dt ? Number(dt.nextElementSibling.textContent.split(' of ')[0].replace(/[^0-9]/g, '')) : NaN;
  });
  const tg = await filterToggle(p);
  row.filter_toggle_ms = med(tg);
  const hv = await hoverFrames(p);
  row.hover_ms_p50 = pct(hv.hover, 0.5);
  row.hover_ms_p95 = pct(hv.hover, 0.95);
  row.hover_frame_ms_p50 = pct(hv.work, 0.5);
  row.hover_frame_ms_p95 = pct(hv.work, 0.95);
  row.frame_ms_p95 = pct(hv.gaps, 0.95);
  row.frame_ms_p99 = pct(hv.gaps, 0.99);
  row.zoom_fps = fps(await zoomFrames(p));
  row.pan_fps = fps(await panFrames(p));
  // Relations tab, first page.
  await p.evaluate(() => (location.hash = location.hash.replace(/\/graph.*$/, '/overview')));
  await p.waitForTimeout(500);
  await p.context().close();
  }
  for (let r = 0; r < runs; r++) {
    const { ctx, p: q } = await newPage(browser, url, style, theme, width);
    if (!args['paths-only']) rel.push((await clickToPaint(q, '.nav a[href*="/relations"]', ['relations:painted'])).ms);
    if (row.first_view === 'overview') {
      // Show all (bounded by the server's limit): the full graph's first paint.
      const pid = await q.evaluate(() => location.hash.split('/')[2]);
      const a = await q.evaluate(async (pid) => {
        const done = window.__painted('graph:built');
        const t0 = performance.now();
        location.hash = `#/p/${pid}/graph?all=1`;
        const { t } = await done;
        return { ms: t - t0, layout: window.__since('graph:layout', t0) - window.__since('graph:data', t0), marks: window.__marksSince(t0) };
      }, pid);
      all.push(a.ms);
      allLayout.push(a.layout);
      allSeg.push(segments(a.marks, a.ms, false));
      allSrc.push(a.marks.source);
    }
    await ctx.close();
  }
  Object.assign(row, {
    tab_click_ms: med(tab),
    graph_response_ms: med(data),
    overview_paint_ms: med(ovPaint),
    open_scope_ms: med(open),
    open_scope_response_ms: med(openData),
    open_scope_layout_ms: med(openLayout),
    open_scope_build_ms: med(openBuild),
    show_all_ms: med(all),
    show_all_layout_ms: med(allLayout),
    relations_first_page_ms: med(rel),
    tab_segments: medSegments(tabSeg),
    tab_layout_source: sources(tabSrc),
    open_scope_segments: medSegments(openSeg),
    open_scope_layout_source: sources(openSrc),
    show_all_segments: medSegments(allSeg),
    show_all_layout_source: sources(allSrc),
  });
  return row;
}

const browser = await chromium.launch(args.chrome ? { executablePath: args.chrome } : {});
const rows = [];
let port = Number(args.port || 7400);
try {
  for (const size of sizes) {
    const { url, child } = await startDashboard(size, port++);
    try {
      for (const style of styles)
        for (const theme of themes)
          for (const width of widths) {
            const row = await benchCombo(browser, url, size, style, theme, width);
            rows.push(row);
            console.log(JSON.stringify(row, (k, v) => (typeof v === 'number' ? Math.round(v * 10) / 10 : v)));
          }
    } finally {
      child.kill();
    }
  }
} finally {
  // A failed step must not leave a browser or a dashboard behind.
  for (const c of children) c.kill();
  await browser.close();
  if (args.json) writeFileSync(args.json, JSON.stringify(rows, null, 1));
}
