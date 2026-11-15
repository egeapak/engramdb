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
  // E2: the Challenged tile counts the store (15), not the capped list (12).
  const tile = await t.p.evaluate(() => {
    const st = [...document.querySelectorAll('.stat')].find((s) => (s.querySelector('.eyebrow') || {}).textContent === 'Challenged');
    return st ? st.querySelector('.v').textContent.trim() : null;
  });
  ok('overview: Challenged tile counts every challenged memory', tile === '15', tile);
  ok('overview: a capped group says how many more', (await text(t, '.att-more')).includes('and 3 more'), await text(t, '.att-more'));
  // E3: memories in use never exceed the live count.
  const inUse = await t.p.evaluate(() => {
    const st = [...document.querySelectorAll('.stat')].find((s) => (s.querySelector('.eyebrow') || {}).textContent === 'Memories in use');
    return st ? [st.querySelector('.v').textContent, st.querySelector('.s').textContent] : null;
  });
  ok('overview: memories in use is at most the live count', inUse && Number(inUse[0]) <= Number(inUse[1].replace(/[^0-9]/g, '')), inUse);
  await go(t, 'memories', '#main table tbody tr');
  const rows0 = (await t.p.$$('#main table tbody tr')).length;
  ok('memories renders rows', rows0 > 0);
  // E1: the store's NaN-criticality row breaks neither a page nor its cursor.
  const more = await t.p.evaluateHandle(() => [...document.querySelectorAll('#main button')].find((b) => /^Load \d+ more$/.test(b.textContent)) || null);
  const hasMore = await more.evaluate((b) => !!b);
  if (hasMore) {
    await more.evaluate((b) => b.click());
    await t.p.waitForTimeout(500);
  }
  const rows1 = (await t.p.$$('#main table tbody tr')).length;
  const errBox = await t.p.$('#main .error-box');
  ok('memories: Load more appends rows past the NaN row', hasMore && rows1 > rows0 && !errBox, { rows0, rows1, hasMore });
  // E6: Type = Hazard, then Status = Invalidated (which holds none): the
  // select still names the type, and "All types" clears it.
  await go(t, 'memories?kind=hazard', '#main table tbody tr');
  await t.p.selectOption('select[aria-label="Status"]', 'invalidated');
  await t.p.waitForTimeout(600);
  const kindShown = await t.p.evaluate(() => {
    const s = document.querySelector('select[aria-label="Type"]');
    return s.options[s.selectedIndex].textContent;
  });
  await t.p.selectOption('select[aria-label="Type"]', '');
  await t.p.waitForTimeout(600);
  const invalidatedRows = await t.p.evaluate(() => [...document.querySelectorAll('#main table tbody tr')].filter((r) => !r.querySelector('td.empty')).length);
  ok('memories: a type filter the status hides still shows, and All types clears it', kindShown === 'Hazard (0)' && invalidatedRows > 0 && !(await t.p.evaluate(() => location.hash)).includes('kind='), { kindShown, invalidatedRows });
  await go(t, 'doctor', '#main .doc-checks, #main h1');
  ok('health renders', (await text(t, '#main')).length > 0);
  await go(t, 'relations', '#main h1');
  ok('relations renders', (await text(t, '#main')).includes('relation'));
  // E9: a malformed escape in the hash is a not-found view, not a stuck page.
  await t.p.evaluate((h) => (location.hash = h), `#/p/${t.pid}/m/%E0%A4%A`);
  await t.p.waitForTimeout(300);
  ok('a malformed link shows a not-found message', (await text(t, '#main .error-box')).includes('This link is not valid'), await text(t, '#main'));
  ok('read-only pages: no console or page error', t.errors.length === 0, t.errors.slice(0, 5));
  await t.ctx.close();

  // E8: a project unavailable when the page loaded comes back with
  // "Try again", without a browser reload.
  const away = join(stores, 'sfault-away');
  renameSync(away, away + '.moved');
  let moved = true;
  try {
    const u = await open(browser, url);
    const id = await u.p.evaluate(() => {
      const o = [...document.querySelectorAll('#proj-select option')].find((x) => x.textContent.startsWith('sfault-away'));
      return o ? o.value : null;
    });
    await u.p.evaluate((h) => (location.hash = h), `#/p/${id}`);
    await u.p.waitForSelector('#main .note.warn button', { timeout: 20000 });
    const said = await text(u, '#main .note.warn');
    renameSync(away + '.moved', away);
    moved = false;
    await u.p.evaluate(() => [...document.querySelectorAll('#main .note.warn button')].find((b) => b.textContent === 'Try again').click());
    const back = await u.p.waitForSelector('#main .stats', { timeout: 20000 }).then(() => true, () => false);
    ok('an unavailable project renders after Try again once it is back', !!id && said.includes('Unavailable') && back, { id, said, back });
    ok('unavailable project page: no console or page error', u.errors.length === 0, u.errors.slice(0, 5));
    await u.ctx.close();
  } finally {
    if (moved) renameSync(away + '.moved', away);
  }
}

// ---------------------------------------------------------------------------
// Write-enabled checks (they review proposed relations)
// ---------------------------------------------------------------------------
/**
 * On the Overview, review a proposed relation through its controls: the
 * item named `key` (its summary and sub line), else the first one that
 * offers `label`. Clicks `label`, then the confirm step. Returns the key.
 */
const review = (p, label, key) =>
  p.evaluate(
    async ([label, key]) => {
      const keyOf = (d) => {
        const li = d.closest('li');
        return li.querySelector('.ttl').textContent + '|' + li.querySelector('.sub').textContent.replace(/ · [^·]*$/, '');
      };
      const ds = [...document.querySelectorAll('details.review-disclosure')].filter((d) => [...d.querySelectorAll('button')].some((b) => b.textContent === label));
      const d = key ? ds.find((x) => keyOf(x) === key) : ds[0];
      if (!d) return null;
      d.open = true;
      const k = keyOf(d);
      [...d.querySelectorAll('button')].find((b) => b.textContent === label).click();
      await new Promise((r) => setTimeout(r, 50));
      [...d.querySelectorAll('button')].find((b) => /^Yes, /.test(b.textContent)).click();
      return k;
    },
    [label, key || null],
  );

async function writable(browser, url) {
  const t = await open(browser, url);
  await go(t, '', '.stats');
  ok('write-enabled overview renders', (await text(t, '.stats')).includes('Memories'));

  // E7: after a review the Overview refreshes and focus lands on the
  // "Needs attention" heading, not <body>.
  const k1 = await review(t.p, 'Confirm');
  await t.p.waitForTimeout(1100);
  const focus = await t.p.evaluate(() => ({ id: document.activeElement.id, tag: document.activeElement.tagName }));
  ok('overview: a review refresh returns focus to the section heading', k1 && focus.id === 'attention-title', { reviewed: k1, focus });

  // E7: leaving within the 600 ms causes no second render of the Overview.
  let stats = 0;
  const onReq = (r) => /\/api\/projects\/[^/]+\/stats$/.test(new URL(r.url()).pathname) && stats++;
  const k2 = await review(t.p, 'Confirm');
  t.p.on('request', onReq);
  await t.p.waitForTimeout(150);
  await t.p.evaluate((h) => (location.hash = h), `#/p/${t.pid}/memories`);
  await t.p.waitForTimeout(1000);
  t.p.off('request', onReq);
  ok('overview: a review refresh does not run after the user left', k2 && stats === 0 && (await t.p.evaluate(() => location.hash)).includes('/memories'), { reviewed: k2, statsRequests: stats });

  // E12: two pages on one proposed relation; A rejects it, then B's
  // confirm finds it already reviewed: a final sentence, no Back, a reload.
  const a = await open(browser, url);
  const b = await open(browser, url);
  await go(a, '', '.stats');
  await go(b, '', '.stats');
  const key = await review(a.p, 'Reject');
  await a.p.waitForTimeout(400);
  const again = await review(b.p, 'Confirm', key);
  await b.p.waitForTimeout(300);
  const seen = await b.p.evaluate(() => ({
    said: [...document.querySelectorAll('details.review-disclosure')].map((d) => d.textContent).find((x) => /Already reviewed elsewhere|no longer exists/.test(x)) || null,
    back: [...document.querySelectorAll('details.review-disclosure button')].some((x) => x.textContent === 'Back'),
  }));
  await b.p.waitForTimeout(900);
  const still = await b.p.evaluate((key) => [...document.querySelectorAll('details.review-disclosure')].some((d) => {
    const li = d.closest('li');
    return li.querySelector('.ttl').textContent + '|' + li.querySelector('.sub').textContent.replace(/ · [^·]*$/, '') === key;
  }), key);
  ok('review: a relation reviewed elsewhere ends in a final state and the list reloads', key && again === key && seen.said && !seen.back && !still, { key, seen, stillListed: still });
  // (B's refused confirm is a 409 the browser logs as a failed resource: expected here.)
  const other = [...a.errors, ...b.errors].filter((e) => !/status of 409 .*\/edges\/confirm$/.test(e));
  ok('second pages: no console or page error', other.length === 0, other.slice(0, 5));
  await a.ctx.close();
  await b.ctx.close();
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
