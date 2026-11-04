// The dashboard's browser bench: runs `engramdb dashboard --no-open` on each
// benchmark store and times the graph and relations views in Chromium,
// from the page's own performance marks and requestAnimationFrame. A
// developer tool (README.md); not embedded, not in CI.
//
//   NODE_PATH=$(npm root -g) node bench/browser/run.mjs --stores /tmp/dash-bench \
//     [--bin target/release/engramdb] [--sizes s60,s1k,s2k,s10k] [--styles plain,grid]
//     [--themes light,dark] [--widths 1280,390] [--runs 5] [--assets web] [--chrome PATH]
//     [--json out.json]
//
// Every figure is a median over `--runs` cold page loads (a fresh browser
// context, so no layout cache) unless it says otherwise.

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fromApi, toInput } from '../layout.mjs';

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
      const out = { ms: t - t0, mark: name };
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

/** Hover 60 memories: hover → painted, and every frame's length meanwhile. */
async function hoverFrames(p) {
  return p.evaluate(async () => {
    const nodes = [...document.querySelectorAll('svg.graph .node')];
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
    for (let i = 0; i < 60; i++) {
      const el = nodes[(i * 7919) % nodes.length];
      await window.__afterPaint();
      const t0 = performance.now();
      if (prev) prev.dispatchEvent(new MouseEvent('mouseleave'));
      el.dispatchEvent(new MouseEvent('mouseenter'));
      hover.push((await window.__afterPaint()) - t0);
      prev = el;
      await new Promise((r) => setTimeout(r, 30));
    }
    run = false;
    // The hover's own frame: the events dispatched at the start of a frame
    // (in a rAF callback), to the task after that frame's paint. That is
    // the main-thread work the 16 ms budget is about, without the wait for
    // the next vsync that `hover` above includes.
    const work = [];
    for (let i = 0; i < 60; i++) {
      const el = nodes[(i * 104729 + 13) % nodes.length];
      await window.__afterPaint();
      work.push(
        await new Promise((res) =>
          requestAnimationFrame(() => {
            const t0 = performance.now();
            if (prev) prev.dispatchEvent(new MouseEvent('mouseleave'));
            el.dispatchEvent(new MouseEvent('mouseenter'));
            const ch = new MessageChannel();
            ch.port1.onmessage = () => res(performance.now() - t0);
            ch.port2.postMessage(0);
          }),
        ),
      );
      prev = el;
      await new Promise((r) => setTimeout(r, 30));
    }
    if (prev) prev.dispatchEvent(new MouseEvent('mouseleave'));
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
  const r = await (await p.$('svg.graph')).boundingBox();
  const cx = r.x + r.width / 2;
  const cy = r.y + r.height / 2;
  return framesDuring(p, async () => {
    await p.mouse.move(cx, cy);
    await p.mouse.down();
    for (let i = 0; i < 60; i++) await p.mouse.move(cx + 120 * Math.sin(i / 9), cy + 60 * Math.cos(i / 9));
    await p.mouse.up();
  });
}

const fps = (gaps) => (gaps.length ? 1000 / (gaps.reduce((a, b) => a + b, 0) / gaps.length) : NaN);

async function benchCombo(browser, url, size, style, theme, width) {
  const row = { size, style, theme, width };
  const tab = [], data = [], ovPaint = [], open = [], openData = [], openLayout = [], openBuild = [], all = [], allLayout = [], rel = [];
  let interactive = null;
  for (let r = 0; r < runs; r++) {
    const { ctx, p } = await newPage(browser, url, style, theme, width);
    // Tab click → first full paint (the overview past 150 memories).
    const t = await clickToPaint(p, '.nav a[href*="/graph"]', ['graph:built', 'graph:overview-built'], ['graph:data', 'graph:layout']);
    tab.push(t.ms);
    data.push(t['graph:data']);
    row.first_view = t.mark === 'graph:overview-built' ? 'overview' : 'graph';
    if (t.mark === 'graph:overview-built') {
      ovPaint.push(t.ms);
      // Open the largest scope from the overview.
      const o = await clickToPaint(p, `.so-scope[aria-label^="${OPEN}:"]`, ['graph:built'], ['graph:data', 'graph:layout', 'graph:built']);
      open.push(o.ms);
      openData.push(o['graph:data']);
      openLayout.push(o['graph:layout'] - o['graph:data']);
      openBuild.push(o['graph:built'] - o['graph:layout']);
    }
    if (r === 0) interactive = p;
    else await ctx.close();
  }
  // The interactive measurements, once, on the view a user lands on: the
  // opened scope past 150 memories, else the whole graph.
  const p = interactive;
  await p.waitForTimeout(500);
  row.shown_nodes = await p.evaluate(() => document.querySelectorAll('svg.graph .node').length);
  row.shown_relations = await p.evaluate(() => document.querySelectorAll('svg.graph .edges > g:not([hidden])').length);
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
  for (let r = 0; r < runs; r++) {
    const { ctx, p: q } = await newPage(browser, url, style, theme, width);
    rel.push((await clickToPaint(q, '.nav a[href*="/relations"]', ['relations:painted'])).ms);
    if (row.first_view === 'overview') {
      // Show all (bounded by the server's limit): the full graph's first paint.
      const pid = await q.evaluate(() => location.hash.split('/')[2]);
      const a = await q.evaluate(async (pid) => {
        const done = window.__painted('graph:built');
        const t0 = performance.now();
        location.hash = `#/p/${pid}/graph?all=1`;
        const { t } = await done;
        return { ms: t - t0, layout: window.__since('graph:layout', t0) - window.__since('graph:data', t0) };
      }, pid);
      all.push(a.ms);
      allLayout.push(a.layout);
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
  });
  return row;
}

/**
 * Layout cold in a Chromium worker: the store's whole graph (up to the
 * server's 2000 cap, past the page's 800 default) through the harness's
 * `toInput`, posted to a fresh `layout-worker.js` per run after a
 * one-memory warm-up, so module loading is not counted.
 */
async function workerLayout(browser, url, size) {
  const { ctx, p } = await newPage(browser, url, 'plain', 'light', 1280);
  const pid = await p.evaluate(() => location.hash.split('/')[2]);
  const token = url.split('#token=')[1];
  const origin = new URL(url).origin;
  const res = await fetch(`${origin}/api/projects/${pid}/graph?limit=2000&include_proposed=true&include_rejected=true`, { headers: { Authorization: `Bearer ${token}` } });
  const g = fromApi(await res.json());
  const out = { size, nodes: g.nodes.length, relations: g.edges.length };
  for (const name of ['scope-force', 'grid']) {
    const input = toInput(g, name);
    const tiny = toInput({ nodes: g.nodes.slice(0, 1), edges: [] }, name);
    const ms = [];
    for (let r = 0; r < runs; r++) {
      ms.push(
        await p.evaluate(
          async ({ input, tiny }) => {
            const w = new Worker(new URL('/js/graph/layout-worker.js', location.origin), { type: 'module' });
            const run = (inp, seq) => new Promise((res, rej) => {
              w.onmessage = (ev) => (ev.data.error ? rej(new Error(ev.data.error)) : res());
              w.postMessage({ seq, input: inp });
            });
            await run(tiny, 1);
            const t0 = performance.now();
            await run(input, 2);
            const t = performance.now() - t0;
            w.terminate();
            return t;
          },
          { input, tiny },
        ),
      );
    }
    out[`layout_cold_${name}_ms`] = med(ms);
  }
  await ctx.close();
  return out;
}

const browser = await chromium.launch(args.chrome ? { executablePath: args.chrome } : {});
const rows = [];
let port = Number(args.port || 7400);
try {
  for (const size of sizes) {
    const { url, child } = await startDashboard(size, port++);
    try {
      if (!args['no-layout']) {
        const lay = await workerLayout(browser, url, size);
        rows.push(lay);
        console.log(JSON.stringify(lay, (k, v) => (typeof v === 'number' ? Math.round(v * 10) / 10 : v)));
      }
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
