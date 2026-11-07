// The graph surface bench: first paint, settle after a zoom, zoom / pan
// frame rates, the hover frame and heap / DOM size of the drawn graph, on
// the views a big store is drawn in (README.md). Where run.mjs walks the
// pages a user clicks through, this one holds the view fixed and compares
// surfaces: run it once with the web assets of each version (`--assets`)
// and diff the JSON. A developer tool; not embedded, not in CI.
//
//   NODE_PATH=$(npm root -g) node bench/browser/views.mjs --stores /tmp/dash-bench \
//     [--bin target/release/engramdb] [--assets web] [--style plain] [--theme light]
//     [--views s1k:all,s2k:all,s10k:open,s10k:all] [--modes cold,cached]
//     [--runs 5] [--cpu-throttle 4] [--gpu] [--dpr 2] [--paint-only] [--label after] [--json out.json]
//
// `cold`: the server's layout cache off and a fresh browser context, so the
// layout is computed (server or worker) on every load; `cached`: one untimed
// load first, so every timed load takes the server's cached layout. Every
// figure is the median over `--runs` loads, each in a fresh context.
//
//   paint_ms    hash change → the frame after `graph:built` (first paint)
//   draw_ms     `graph:layout` → that frame: what the surface costs
//   settle_ms   a wheel zoom's settle timer firing → the frame after the
//               last settle step (on the SVG surface a second 160 ms timer
//               redraws the relations, and its wait is included: the
//               settled picture is not on screen before)
//   settle_work_ms   the main-thread work of those steps: each timer's
//               script with its microtasks, plus the frame after it (its
//               rAF callbacks, where the canvas draws, and the raster the
//               canvas pays at paint), from its rAF to the task after it
//   settle_long  long tasks (> 50 ms) from the settle until 1 s after it
//   pan_settle_ms / pan_settle_work_ms / pan_settle_long   the same, for
//               the settle after the drag (a pan only, no zoom)
//   focus_settle_ms / focus_settle_work_ms / focus_settle_long   the same,
//               for a zoom settle with a memory selected (the faded copy)
//   toggle_ms / toggle_work_ms / toggle_long   a display filter
//               (Proposed) switched off and on again: click → the task after
//               the next frame, its work measured like a settle step's,
//               the mean of the two (toggle_script_ms: the click's script;
//               toggle_layer_ms: the canvas host's own base bring-up in it)
//   paint_long / paint_busy_ms   long tasks in the 1 s after the first
//               paint, and their summed time (what is left to do then: the
//               accessible list, the margin, the hidden layer)
//   layer_ms_base / layer_ms_dim   (canvas) the host's own timing of its last
//               base / faded-copy bring-up, parts included (graphDebug)
//   backing_mb  (canvas) every canvas the host holds, 4 bytes per device px
//   settle_base_ms / idle_dim_ms (+ _parts_ms)   (canvas) the zoom settle's
//               base bring-up, and the faded copy composed after it in idle
//               time, with the share spent drawing parts
//   zoom_fps / pan_fps   one wheel event per frame, 40 in and 40 out / a
//               60-step mouse drag (a pan is a composited gesture on the
//               canvas surface, a view-box change per move on the SVG)
//   hover_frame_ms   a hover dispatched at the start of a frame → the task
//               after that frame's paint (p50 of 30)
//   heap_mb, dom_nodes, layout_objects   after a forced GC, via CDP

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
if (!args.stores) {
  console.error('usage: node bench/browser/views.mjs --stores <dir from `cargo run --example dashboard_bench_store`> [options]');
  process.exit(2);
}
const stores = resolve(args.stores);
const bin = args.bin ? resolve(args.bin) : fileURLToPath(new URL('../../../../target/release/engramdb', import.meta.url));
const views = list('views', 's1k:all,s2k:all,s10k:open,s10k:all').map((v) => v.split(':'));
const modes = list('modes', 'cold,cached');
const runs = Number(args.runs || 5);
const style = args.style || 'plain';
const theme = args.theme || 'light';
const throttle = Number(args['cpu-throttle'] || 0);
const OPEN = args.scope || 'storage';

const med = (a) => {
  const s = a.filter((x) => Number.isFinite(x)).sort((x, y) => x - y);
  return s.length ? s[s.length >> 1] : NaN;
};
const fps = (gaps) => (gaps.length ? 1000 / (gaps.reduce((a, b) => a + b, 0) / gaps.length) : NaN);

const children = new Set();
function startDashboard(size, port, cold) {
  const env = {
    ...process.env,
    ENGRAMDB_DATA_DIR: join(stores, 'data'),
    ENGRAMDB_CONFIG_DIR: join(stores, 'conf'),
    ENGRAMDB_REGISTRY_PATH: join(stores, 'conf', 'registry.json'),
    ENGRAMDB_MODEL_CACHE_DIR: join(stores, 'models'),
    ENGRAMDB_OFFLINE: '1',
    ENGRAMDB_IN_PROCESS: '1',
    ENGRAMDB_DASHBOARD_CACHE_DIR: join(stores, 'cache'),
    ...(cold ? { ENGRAMDB_DASHBOARD_LAYOUT_CACHE: '0' } : {}),
  };
  const argv = ['dashboard', '--no-open', '--json', '--port', String(port)];
  if (args.assets) argv.push('--assets-dir', resolve(args.assets));
  const child = spawn(bin, argv, { cwd: join(stores, size), env, stdio: ['ignore', 'pipe', 'inherit'] });
  children.add(child);
  child.on('exit', () => children.delete(child));
  return new Promise((res, rej) => {
    let out = '';
    const timer = setTimeout(() => rej(new Error(`no token URL within 30 s: ${out}`)), 30000);
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

// In the page: paint waiters on performance marks (as run.mjs), and every
// 160 ms timer (the gesture and settle timers of views/graph.js) wrapped so
// a settle's script time and the paint after it are recorded.
function initScript() {
  const orig = performance.mark.bind(performance);
  const waiters = new Map();
  window.__afterPaint = () =>
    new Promise((r) =>
      requestAnimationFrame(() => {
        const ch = new MessageChannel();
        ch.port1.onmessage = () => r(performance.now());
        ch.port2.postMessage(0);
      }),
    );
  performance.mark = (name, o) => {
    const r = orig(name, o);
    const w = waiters.get(name);
    if (w) {
      waiters.delete(name);
      for (const f of w) window.__afterPaint().then(f);
    }
    return r;
  };
  window.__painted = (...names) =>
    new Promise((res) => {
      for (const n of names) waiters.set(n, [...(waiters.get(n) || []), (t) => res({ name: n, t })]);
    });
  window.__mark = (name, t0) => {
    const e = performance.getEntriesByName(name).filter((x) => x.startTime >= t0).pop();
    return e ? e.startTime : NaN;
  };
  window.__long = [];
  try {
    new PerformanceObserver((l) => {
      for (const e of l.getEntries()) window.__long.push({ t: e.startTime, d: e.duration });
    }).observe({ type: 'longtask', buffered: true });
  } catch (_) {}
  // Long tasks overlapping [t0, t1], and their summed time.
  window.__longIn = (t0, t1) => {
    const l = window.__long.filter((e) => e.t + e.d > t0 && e.t < t1);
    return { n: l.length, ms: l.reduce((a, e) => a + e.d, 0) };
  };
  // Run `fn` and time it to the end of the frame that shows what it did:
  // `script` is fn with its microtasks (to a message posted after it, or the
  // frame's start if that comes first), `frame` that frame from its start
  // (its rAF callbacks, where the canvas host draws, then the paint and the
  // raster a canvas pays there) to the task after it. Resolves
  // { t0, script, frame, work, t2 }.
  window.__timed = (fn) =>
    new Promise((res) => {
      const t0 = performance.now();
      fn();
      let t1 = NaN;
      const ch = new MessageChannel();
      ch.port1.onmessage = () => {
        if (!Number.isFinite(t1)) t1 = performance.now();
      };
      ch.port2.postMessage(0);
      requestAnimationFrame((tr) => {
        if (!Number.isFinite(t1)) t1 = Math.max(tr, t0);
        const c2 = new MessageChannel();
        c2.port1.onmessage = () => {
          const t2 = performance.now();
          const script = t1 - t0;
          const frame = t2 - Math.max(tr, t1);
          res({ t0, script, frame, work: script + frame, t2 });
        };
        c2.port2.postMessage(0);
      });
    });
  // After a gesture: wait for its settle timers (and 1 s more), then sum them.
  window.__settled = async () => {
    await new Promise((res) => ost(res, 1300));
    const s = window.__settles;
    const t0 = s.length ? s[0].t0 : NaN;
    const t2 = s.length ? s[s.length - 1].t2 : NaN;
    const long = window.__longIn(t0, t2 + 1000).n;
    window.__settles = [];
    return { settle: t2 - t0, t0, t2, work: s.reduce((a, x) => a + x.work, 0), steps: s.length, long };
  };
  // The next pointerup, timed like a settle step (a pan settles on it): the
  // capture listener runs first, so the message posted from it follows the
  // page's own handler and its microtasks.
  window.__armUp = () =>
    new Promise((res) =>
      window.addEventListener('pointerup', () => window.__timed(() => {}).then(res), { capture: true, once: true }),
    );
  window.__settles = [];
  const ost = window.setTimeout.bind(window);
  window.setTimeout = (fn, ms, ...rest) => {
    if (ms !== 160 || typeof fn !== 'function') return ost(fn, ms, ...rest);
    return ost(() => {
      const rec = { t0: performance.now(), script: NaN, work: NaN, t2: NaN };
      window.__settles.push(rec);
      window.__timed(() => fn(...rest)).then((r) => Object.assign(rec, r));
    }, ms);
  };
}

/** Wait until the dashboard answers its page (the token URL is printed before the listener serves). */
async function ready(url) {
  const base = url.split('#')[0];
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(base)).ok) return;
    } catch (_) {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`dashboard not answering: ${base}`);
}

async function load(browser, url, size, view) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 }, deviceScaleFactor: Number(args.dpr || 1), colorScheme: theme });
  await ctx.addInitScript(initScript);
  await ctx.addInitScript((st) => {
    try {
      localStorage.setItem('engramdb.graphStyle', st);
    } catch (_) {}
  }, style);
  const p = await ctx.newPage();
  const cdp = await ctx.newCDPSession(p);
  if (throttle) await cdp.send('Emulation.setCPUThrottlingRate', { rate: throttle });
  p.on('pageerror', (e) => console.error('pageerror', e.message));
  await ready(url);
  await p.goto(url);
  await p.waitForSelector('.nav a[href*="/graph"]');
  await p.waitForTimeout(200);
  const q = view === 'all' ? 'all=1' : view === 'open' ? `top_scope=${OPEN}` : '';
  const paint = await p.evaluate(async (q) => {
    const pid = location.hash.split('/')[2];
    const done = window.__painted('graph:built');
    const t0 = performance.now();
    location.hash = `#/p/${pid}/graph${q ? '?' + q : ''}`;
    const { t } = await done;
    const lay = window.__mark('graph:layout', t0);
    const src = performance.getEntriesByName('graph:layout').filter((x) => x.startTime >= t0).pop();
    await new Promise((r) => setTimeout(r, 1100));
    const after = window.__longIn(t, t + 1000);
    return { paint: t - t0, draw: t - lay, data: window.__mark('graph:data', t0) - t0, paint_long: after.n, paint_busy_ms: after.ms, source: src && src.detail ? src.detail.source : null };
  }, q);
  return { ctx, p, cdp, paint };
}

async function interact(p, cdp) {
  await p.waitForTimeout(400);
  const out = {};
  out.surface = await p.evaluate(() => (document.querySelector('.g-canvas') ? 'canvas' : 'svg'));
  Object.assign(out, await heapAndDom(cdp));
  // zoom: one wheel event per frame, then the settle
  await p.focus('.graph-box');
  const z = await p.evaluate(async () => {
    const box = document.querySelector('.graph-box');
    const r = box.getBoundingClientRect();
    const gaps = [];
    let last = 0;
    window.__settles = [];
    for (let i = 0; i < 80; i++) {
      const t = await new Promise((res) => requestAnimationFrame(res));
      if (last) gaps.push(t - last);
      last = t;
      // in, then out, ending 1.5 steps in: a view that differs from Fit
      box.dispatchEvent(new WheelEvent('wheel', { deltaY: i < 43 ? -60 : 60, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, bubbles: true, cancelable: true }));
    }
    return { gaps: gaps.slice(1), ...(await window.__settled()) };
  });
  // the hidden layer's bring-up after that settle (idle time, no focus)
  Object.assign(
    out,
    await p.evaluate(() => {
      const c = document.querySelector('.g-canvas');
      const st = c && c.graphDebug && c.graphDebug.stats;
      if (!st || !st.ms) return {};
      return { idle_dim_ms: st.ms.dim, idle_dim_parts_ms: st.partMs.dim, settle_base_ms: st.ms.base, settle_base_parts_ms: st.partMs.base };
    }),
  );
  out.zoom_fps = fps(z.gaps);
  out.settle_ms = z.settle;
  out.settle_work_ms = z.work;
  out.settle_long = z.long;
  out.settle_steps = z.steps;
  // pan: a real mouse drag
  const r = await (await p.$('.graph-box')).boundingBox();
  const cx = r.x + r.width / 2;
  const cy = r.y + r.height / 2;
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
  await p.evaluate(() => {
    window.__up = window.__armUp();
  });
  await p.mouse.move(cx, cy);
  await p.mouse.down();
  for (let i = 0; i < 60; i++) await p.mouse.move(cx + 120 * Math.sin(i / 9), cy + 60 * Math.cos(i / 9));
  await p.mouse.up();
  out.pan_fps = fps(await p.evaluate(() => ((window.__run = false), window.__gaps.slice(2))));
  {
    const ps = await p.evaluate(async () => {
      const up = await window.__up;
      const rest = await window.__settled();
      // an SVG settle runs on a 160 ms timer after the pointerup
      const end = Number.isFinite(rest.t2) ? Math.max(up.t2, rest.t2) : up.t2;
      return { settle: end - up.t0, work: up.work + rest.work, long: window.__longIn(up.t0, end + 1000).n };
    });
    out.pan_settle_ms = ps.settle;
    out.pan_settle_work_ms = ps.work;
    out.pan_settle_long = ps.long;
  }
  await p.keyboard.press('0');
  await p.waitForTimeout(500);
  // a display filter off and on again
  {
    const tg = await p.evaluate(async () => {
      const chip = () => [...document.querySelectorAll('.chip')].find((b) => b.textContent === 'Proposed');
      if (!chip()) return null;
      const rs = [];
      for (let k = 0; k < 2; k++) {
        await window.__afterPaint();
        rs.push(await window.__timed(() => chip().click()));
        const c = document.querySelector('.g-canvas');
        rs[rs.length - 1].layer = c && c.graphDebug && c.graphDebug.stats && c.graphDebug.stats.ms ? c.graphDebug.stats.ms.base : NaN;
        await new Promise((r) => setTimeout(r, 1200));
      }
      return { ms: (rs[0].t2 - rs[0].t0 + rs[1].t2 - rs[1].t0) / 2, work: (rs[0].work + rs[1].work) / 2, script: (rs[0].script + rs[1].script) / 2, layer: (rs[0].layer + rs[1].layer) / 2, all: rs.map((r) => [Math.round(r.script), Math.round(r.frame)]).join(' '), long: window.__longIn(rs[0].t0, rs[1].t2 + 1000).n };
    });
    if (tg) {
      out.toggle_ms = tg.ms;
      out.toggle_work_ms = tg.work;
      out.toggle_long = tg.long;
      out.toggle_script_ms = tg.script;
      out.toggle_layer_ms = tg.layer;
    }
  }
  // the hover frame (as run.mjs's hoverFrames `work`)
  out.hover_frame_ms = med(
    await p.evaluate(async () => {
      const o = document.querySelector('.graph-box').getBoundingClientRect();
      const nodes = [...document.querySelectorAll('svg.graph .nodes .node, .g-a11y-node')]
        .map((el) => {
          const b = el.getBoundingClientRect();
          return { el, x: b.left + b.width / 2, y: b.top + b.height / 2 };
        })
        .filter((n) => n.x > o.left + 8 && n.x < o.right - 8 && n.y > o.top + 8 && n.y < o.bottom - 8);
      const host = document.querySelector('.g-canvas');
      const move = (to, from) => {
        if (host) {
          const at = to || { x: o.left + 2, y: o.top + 2 };
          host.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, clientX: at.x, clientY: at.y, pointerType: 'mouse' }));
          return;
        }
        if (from) from.el.dispatchEvent(new MouseEvent('mouseout', { bubbles: true, relatedTarget: to && to.el }));
        if (to) to.el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true, relatedTarget: from && from.el }));
      };
      const work = [];
      let prev = null;
      for (let i = 0; i < 30; i++) {
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
      return work;
    }),
  );
  // a zoom settle with a memory selected
  {
    const fs = await p.evaluate(async () => {
      const box = document.querySelector('.graph-box');
      const o = box.getBoundingClientRect();
      const el = [...document.querySelectorAll('svg.graph .nodes .node, .g-a11y-node')].find((e) => {
        const b = e.getBoundingClientRect();
        return Math.abs(b.left + b.width / 2 - (o.left + o.width / 2)) < o.width / 4 && Math.abs(b.top + b.height / 2 - (o.top + o.height / 2)) < o.height / 4;
      });
      if (!el) return null;
      const b = el.getBoundingClientRect();
      const at = { clientX: b.left + b.width / 2, clientY: b.top + b.height / 2, bubbles: true };
      const host = document.querySelector('.g-canvas');
      if (host) {
        host.dispatchEvent(new PointerEvent('pointermove', { ...at, pointerType: 'mouse' }));
        host.dispatchEvent(new PointerEvent('pointerdown', { ...at, pointerType: 'mouse', button: 0 }));
        host.dispatchEvent(new PointerEvent('pointerup', { ...at, pointerType: 'mouse', button: 0 }));
        host.dispatchEvent(new MouseEvent('click', at));
      } else el.dispatchEvent(new MouseEvent('click', at));
      await new Promise((r) => setTimeout(r, 1200));
      window.__settles = [];
      for (let i = 0; i < 6; i++) {
        await new Promise((res) => requestAnimationFrame(res));
        box.dispatchEvent(new WheelEvent('wheel', { deltaY: -60, clientX: o.left + o.width / 2, clientY: o.top + o.height / 2, bubbles: true, cancelable: true }));
      }
      return window.__settled();
    });
    if (fs) {
      out.focus_settle_ms = fs.settle;
      out.focus_settle_work_ms = fs.work;
      out.focus_settle_long = fs.long;
    }
  }
  const dbg = await p.evaluate(() => {
    const c = document.querySelector('.g-canvas');
    const d = c && c.graphDebug;
    if (!d) return null;
    return { base: d.stats && d.stats.ms ? d.stats.ms.base : NaN, dim: d.stats && d.stats.ms ? d.stats.ms.dim : NaN, mb: d.backingBytes ? d.backingBytes() / 2 ** 20 : NaN };
  });
  if (dbg) {
    out.layer_ms_base = dbg.base;
    out.layer_ms_dim = dbg.dim;
    out.backing_mb = dbg.mb;
  }
  return out;
}

async function heapAndDom(cdp) {
  await cdp.send('Performance.enable');
  await cdp.send('HeapProfiler.collectGarbage');
  const { metrics } = await cdp.send('Performance.getMetrics');
  const m = Object.fromEntries(metrics.map((x) => [x.name, x.value]));
  return { heap_mb: m.JSHeapUsedSize / 2 ** 20, dom_nodes: m.Nodes, layout_objects: m.LayoutObjects };
}

// `--gpu`: SwiftShader as the GPU, so canvas and SVG raster in the GPU
// process as on a machine with one (headless Chromium otherwise rasters a
// 2D canvas on the main thread, at its paint, and SVG on raster threads).
const launch = { ...(args.chrome ? { executablePath: args.chrome } : {}), ...(args.gpu ? { args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--enable-gpu-rasterization', '--ignore-gpu-blocklist'] } : {}) };
const browser = await chromium.launch(launch);
const rows = [];
let port = Number(args.port || 7480);
try {
  for (const mode of modes)
    for (const [size, view] of views) {
      const { url, child } = await startDashboard(size, port++, mode === 'cold');
      try {
        if (mode === 'cached') {
          const w = await load(browser, url, size, view);
          await w.ctx.close();
        }
        const samples = [];
        for (let r = 0; r < runs; r++) {
          const { ctx, p, cdp, paint } = await load(browser, url, size, view);
          samples.push({ ...paint, ...(args['paint-only'] ? {} : await interact(p, cdp)) });
          await ctx.close();
        }
        const row = { label: args.label || '', size, view, mode, style, theme, throttle, gpu: !!args.gpu, dpr: Number(args.dpr || 1), runs, surface: samples[0].surface, source: samples.map((s) => s.source).join(',') };
        for (const k of Object.keys(samples[0])) if (typeof samples[0][k] === 'number') row[k] = med(samples.map((s) => s[k]));
        rows.push(row);
        console.log(JSON.stringify(row, (k, v) => (typeof v === 'number' ? Math.round(v * 10) / 10 : v)));
      } finally {
        child.kill();
      }
    }
} finally {
  for (const c of children) c.kill();
  await browser.close();
  if (args.json) writeFileSync(args.json, JSON.stringify(rows, null, 1));
}
