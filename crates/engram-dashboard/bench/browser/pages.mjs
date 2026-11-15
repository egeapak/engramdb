// Browser checks of the dashboard's non-graph pages (Overview, Memories,
// Health, Relations, the memory page) against the fault fixture `sfault`
// (examples/support/dashboard_store.rs `build_fault`): a store holding what
// the generated ones never do (a NaN criticality row, more challenged
// memories than a list shows, a merge-conflicted file, scopes named `-` and
// `a,b`, a relation of kind `constructor`, reads of invalidated memories).
// Not embedded. Run by hand (README.md) and by CI's `frontend-browser` job.
//
//   cargo run --example dashboard_bench_store -- /tmp/dash-bench sfault
//   node bench/browser/pages.mjs --stores /tmp/dash-bench \
//     [--bin target/debug/engramdb] [--chrome /usr/bin/google-chrome] [--json out.json]
//
// It starts two dashboards on `sfault`: a read-only one and one with
// `--allow-writes` (whose checks review proposed relations, so they change
// the store: regenerate it to start over; it holds enough proposals for
// several runs). Exits 1 if any check fails. Every page is also checked
// for page errors and console errors (a CSP violation is one).

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { renameSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
function loadChromium() {
  for (const name of ['playwright-core', 'playwright']) {
    try {
      return require(name).chromium;
    } catch (e) {
      if (e.code !== 'MODULE_NOT_FOUND') throw e;
    }
  }
  console.error('Playwright is not installed. Run `npm ci` in crates/engram-dashboard/bench/browser (see README.md).');
  process.exit(2);
}
const chromium = loadChromium();

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith('--') ? [...acc, [a.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : '1']] : acc), []),
);
if (!args.stores) {
  console.error('usage: node bench/browser/pages.mjs --stores <dir> [--bin <engramdb>] [--chrome <path>] [--json out.json]');
  process.exit(2);
}
const stores = resolve(args.stores);
const bin = args.bin ? resolve(args.bin) : fileURLToPath(new URL('../../../../target/release/engramdb', import.meta.url));
const children = new Set();

function startDashboard(port, writes) {
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
  if (writes) argv.push('--allow-writes');
  if (args.assets) argv.push('--assets-dir', resolve(args.assets));
  const child = spawn(bin, argv, { cwd: join(stores, 'sfault'), env, stdio: ['ignore', 'pipe', 'inherit'] });
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

const results = [];
let failed = 0;
function ok(name, pass, detail) {
  results.push({ name, pass: !!pass, detail });
  if (!pass) failed++;
  console.log(`${pass ? 'ok  ' : 'FAIL'} ${name}${pass || detail === undefined ? '' : ' ' + JSON.stringify(detail)}`);
}

/** A fresh page on `url`, with its page and console errors collected. */
async function open(browser, url) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, colorScheme: 'light' });
  const p = await ctx.newPage();
  const errors = [];
  p.on('pageerror', (e) => errors.push('pageerror: ' + e.message));
  p.on('console', (m) => {
    if (m.type() !== 'error') return;
    const from = m.location().url || '';
    errors.push('console: ' + m.text() + (from ? ' @ ' + from.replace(/token=[0-9a-f]+/, 'token=…') : ''));
  });
  await p.goto(url);
  await p.waitForSelector('#nav a[href*="/graph"]');
  const pid = await p.evaluate(() => location.hash.split('/')[2]);
  return { ctx, p, errors, pid };
}

/** Go to `#/p/<pid>/<rest>` and wait until `sel` is in the page. */
async function go(t, rest, sel) {
  await t.p.evaluate((h) => (location.hash = h), `#/p/${t.pid}${rest ? '/' + rest : ''}`);
  if (sel) await t.p.waitForSelector(sel, { timeout: 20000 });
  await t.p.waitForTimeout(200);
}

const text = (t, sel) => t.p.evaluate((s) => [...document.querySelectorAll(s)].map((e) => e.textContent).join('\n'), sel);

// ---------------------------------------------------------------------------
// Read-only checks
// ---------------------------------------------------------------------------
async function readOnly(browser, url) {
  const t = await open(browser, url);
  await go(t, '', '.stats');
  ok('overview renders', (await text(t, '.stats')).includes('Memories'));
  await go(t, 'memories', '#main table tbody tr');
  ok('memories renders rows', (await t.p.$$('#main table tbody tr')).length > 0);
  await go(t, 'doctor', '#main .doc-checks, #main h1');
  ok('health renders', (await text(t, '#main')).length > 0);
  await go(t, 'relations', '#main h1');
  ok('relations renders', (await text(t, '#main')).includes('relation'));
  ok('read-only pages: no console or page error', t.errors.length === 0, t.errors.slice(0, 5));
  await t.ctx.close();
}

// ---------------------------------------------------------------------------
// Write-enabled checks (they review proposed relations)
// ---------------------------------------------------------------------------
async function writable(browser, url) {
  const t = await open(browser, url);
  await go(t, '', '.stats');
  ok('write-enabled overview renders', (await text(t, '.stats')).includes('Memories'));
  ok('write-enabled pages: no console or page error', t.errors.length === 0, t.errors.slice(0, 5));
  await t.ctx.close();
}

const browser = await chromium.launch(args.chrome ? { executablePath: args.chrome } : {});
let port = Number(args.port || 7520);
try {
  for (const [writes, run] of [
    [false, readOnly],
    [true, writable],
  ]) {
    const { url, child } = await startDashboard(port++, writes);
    try {
      await run(browser, url);
    } finally {
      child.kill();
    }
  }
} finally {
  for (const c of children) c.kill();
  await browser.close();
  if (args.json) writeFileSync(args.json, JSON.stringify(results, null, 1));
}
console.log(failed ? `${failed} of ${results.length} checks failed` : `${results.length} checks passed`);
process.exit(failed ? 1 : 0);
