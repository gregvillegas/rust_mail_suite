// Admin console end-to-end test (Phase 7): a real browser against the real server,
// database, Postfix, Dovecot and Amavis.
//
//   make mta-up              # the dev mail stack must be running
//   cd scripts/browser-smoke && npm install && npm run admin
//
// A super administrator creates a domain and three users in the UI; all three sign in,
// the quota set in the UI is enforced by Dovecot, a domain administrator stays inside
// their domain, and the CSV import, audit log, charts and health page are exercised.
// It creates throw-away domains in the dev database and removes them again. Needs:
// psql, docker, Chrome (CHROME_PATH), built rustmail-web and rustmail-cli binaries.
import { chromium } from 'playwright-core';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import os from 'node:os';
import net from 'node:net';
import crypto from 'node:crypto';

const R = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const WEB = `${R}/target/debug/rustmail-web`;
const CLI = `${R}/target/debug/rustmail-cli`;
const DB_URL = process.env.DATABASE_URL || 'postgres://rustmail:rustmail@localhost:5433/rustmail';
const CHROME = process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const SHOTS = process.env.E2E_SHOTS || path.join(os.tmpdir(), 'rustmail-admin-smoke');
mkdirSync(SHOTS, { recursive: true });
const RUN = Date.now();
const HOME_DOMAIN = `adm${RUN}.test`; // where the administrators live
const OTHER_DOMAIN = `oth${RUN}.test`; // a second tenant
const NEW_DOMAIN = `new${RUN}.test`; // created in the UI
const PW = 'correct-horse-battery-1';
const VMAIL = path.join(R, 'var/vhosts');
const MASTER_PASSWORD = (readFileSync(`${R}/deploy/dev/mta.env`, 'utf8').match(/^MAIL_MASTER_PASSWORD=(.+)$/m) || [])[1];
if (!MASTER_PASSWORD) throw new Error('MAIL_MASTER_PASSWORD not found in deploy/dev/mta.env');

let pass = 0;
const failures = [];
function check(label, ok, detail = '') {
  if (ok) pass++; else failures.push(`${label} ${detail}`);
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${label}${ok || !detail ? '' : '  -> ' + String(detail).slice(0, 300)}`);
}
const section = (t) => console.log(`\n${t}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', ...opts });
  if (r.status !== 0 && !opts.allowFail) throw new Error(`${cmd} ${args.join(' ')} failed: ${r.stderr || r.stdout}`);
  return r;
}
function psql(sql) {
  const url = new URL(DB_URL);
  return run('psql', ['-h', url.hostname, '-p', url.port || '5432', '-U', url.username, '-q', '-tA', '-d', url.pathname.slice(1), '-c', sql],
    { env: { ...process.env, PGPASSWORD: decodeURIComponent(url.password) }, allowFail: true });
}
function cli(args) {
  return run(CLI, ['--database-url', DB_URL, '--vmail-base', VMAIL, ...args], { cwd: os.tmpdir() });
}
/** Delivers a raw message through Postfix's port 25 inside the container. */
function deliver(to, raw) {
  const r = run('docker', ['exec', '-i', 'rustmail-mta', 'swaks', '--server', '127.0.0.1', '--from', 'outsider@example.org', '--to', to, '--data', '-', '--hide-all'], { input: raw, allowFail: true });
  return r.status === 0;
}
function message(to, subject, kilobytes = 0) {
  const filler = kilobytes ? `${'x'.repeat(76)}\r\n`.repeat(Math.ceil((kilobytes * 1024) / 78)) : '';
  return `From: Olivia Outsider <outsider@example.org>\r\nTo: ${to}\r\nSubject: ${subject}\r\nMessage-ID: <${crypto.randomUUID()}@example.org>\r\nDate: Tue, 11 Nov 2025 09:30:00 +0000\r\n\r\nHello.\r\n${filler}`;
}
function freePort() {
  return new Promise((res) => { const s = net.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); });
}
async function startServer() {
  const port = await freePort();
  const proc = spawn(WEB, [], {
    cwd: os.tmpdir(),
    env: {
      PATH: process.env.PATH, DATABASE_URL: DB_URL, JWT_SECRET: crypto.randomBytes(32).toString('hex'),
      WEB_BIND_ADDR: `127.0.0.1:${port}`, COOKIE_SECURE: 'false', RUST_LOG: 'warn', ASSET_SOURCE: 'local',
      LOGIN_RATE_BURST: '100', LOGIN_RATE_PERIOD_SECS: '1', VMAIL_BASE: VMAIL, VMAIL_DIR_MODE: '0700',
      IMAP_HOST: '127.0.0.1', IMAP_PORT: '1993', SMTP_HOST: '127.0.0.1', SMTP_PORT: '5587',
      MAIL_TLS_INSECURE: '1', MAIL_MASTER_USER: 'webmail', MAIL_MASTER_PASSWORD: MASTER_PASSWORD,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  proc.stdout.on('data', (d) => (log += d)); proc.stderr.on('data', (d) => (log += d));
  const url = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 80; i++) {
    try { if ((await fetch(url + '/healthz')).ok) break; } catch { /* not up yet */ }
    await sleep(250);
  }
  return { url, stop: () => proc.kill('SIGINT'), log: () => log };
}
function watch(page) {
  const problems = [];
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    // The test opens pages it expects to be refused; the browser logs those 403s itself.
    if (/Failed to load resource: the server responded with a status of (40[34]|422)/.test(m.text())) return;
    problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`));
  return problems;
}
async function login(page, base, email, password = PW) {
  await page.goto(base + '/login');
  await page.locator('input[name="email"]').fill(email);
  await page.locator('#login-password').fill(password);
  await page.locator('#login-form button[type="submit"]').click();
  await page.waitForURL(base + '/', { timeout: 15000 });
}
async function eventually(fn, seconds = 40) {
  for (let i = 0; i < seconds * 2; i++) {
    try { const v = await fn(); if (v) return v; } catch { /* retry */ }
    await sleep(500);
  }
  return false;
}
/** Submits the form with `button` and waits for the htmx response. */
async function submit(page, urlPart, button) {
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.request().method() === 'POST' && r.url().includes(urlPart), { timeout: 20000 }),
    button.click(),
  ]);
  return response;
}

// ── setup ───────────────────────────────────────────────────────────────────
section('Setup');
cli(['domain', 'create', HOME_DOMAIN]);
cli(['domain', 'create', OTHER_DOMAIN]);
cli(['user', 'create', `root@${HOME_DOMAIN}`, '--password', PW, '--role', 'super-admin', '--name', 'Root']);
cli(['user', 'create', `dana@${HOME_DOMAIN}`, '--password', PW, '--role', 'domain-admin', '--name', 'Dana']);
cli(['user', 'create', `zed@${OTHER_DOMAIN}`, '--password', PW, '--name', 'Zed']);
check('administrators and a second tenant created', true);

const server = await startServer();
const browser = await chromium.launch({ executablePath: CHROME, headless: true });
const createdDomains = [HOME_DOMAIN, OTHER_DOMAIN, NEW_DOMAIN];
try {
  const rootCtx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US', acceptDownloads: true });
  const page = await rootCtx.newPage();
  const problems = watch(page);
  await login(page, server.url, `root@${HOME_DOMAIN}`);

  // ── domain and users ──────────────────────────────────────────────────────
  section('Domain and users (super administrator)');
  await page.goto(server.url + '/admin');
  await page.getByRole('link', { name: 'Domains', exact: true }).click();
  await page.waitForURL('**/admin/domains');
  check('the domain list shows the existing domains', (await page.locator('table').innerText()).includes(HOME_DOMAIN));
  await page.locator('#d-name').fill(NEW_DOMAIN);
  await page.locator('#d-quota').fill('50');
  const created = await submit(page, '/admin/domains', page.getByRole('button', { name: 'Create' }));
  check('creating a domain redirects to it', created.ok() && /hx-redirect/i.test(JSON.stringify(created.headers())));
  await page.waitForURL(/\/admin\/domains\/[0-9a-f-]+\?done=created/);
  check('the new domain page confirms and shows the default quota', (await page.getByRole('status').first().innerText()).includes('Created') && (await page.locator('dl').innerText()).includes('50.0 MB'));
  const domainUrl = page.url().split('?')[0];

  const users = [
    { local: 'tiny', quota: '1', password: '' }, // generated password, 1 MB quota
    { local: 'chosen', quota: '', password: 'chosen-password-123' },
    { local: 'plain', quota: '', password: '' },
  ];
  for (const u of users) {
    await page.goto(server.url + '/admin/users/new');
    await page.locator('#n-local').fill(u.local);
    await page.locator('#n-domain').selectOption(NEW_DOMAIN);
    await page.locator('#n-name').fill(u.local.toUpperCase());
    if (u.quota) await page.locator('#n-quota').fill(u.quota);
    if (u.password) await page.locator('#n-password').fill(u.password);
    const r = await submit(page, '/admin/users', page.getByRole('button', { name: 'Create' }));
    check(`creating ${u.local} succeeds`, r.ok(), String(r.status()));
    if (u.password) {
      await page.getByText('The password you entered was set.').waitFor();
    } else {
      u.password = (await page.locator('[data-testid="generated-password"]').innerText()).trim();
      check(`${u.local}: a generated password is shown once`, u.password.length >= 12);
    }
    u.email = `${u.local}@${NEW_DOMAIN}`;
    check(`${u.local}: the Maildir exists on disk`, existsSync(path.join(VMAIL, NEW_DOMAIN, u.local)));
  }
  await page.screenshot({ path: `${SHOTS}/01-user-created.png` });

  section('All three users sign in');
  for (const u of users) {
    const c = await browser.newContext({ locale: 'en-US' });
    const p = await c.newPage();
    const pp = watch(p);
    let ok = true;
    try { await login(p, server.url, u.email, u.password); } catch { ok = false; }
    check(`${u.email} signs in with the password from the console`, ok);
    if (ok) {
      await p.goto(server.url + '/mail');
      check(`${u.email} reaches the mailbox (Dovecot knows the account)`, await p.locator('#mail-folders').isVisible({ timeout: 15000 }).catch(() => false));
      const denied = await p.goto(server.url + '/admin/users');
      check(`${u.email} (plain user) is kept out of the console`, denied.status() === 403);
    }
    check(`${u.email}: no console errors`, pp.length === 0, pp.join(' | '));
    await c.close();
  }

  // ── quota ─────────────────────────────────────────────────────────────────
  section('Quota is enforced and visible');
  const tiny = users[0];
  await page.goto(server.url + '/admin/users?q=tiny');
  await page.getByRole('link', { name: tiny.email }).click();
  check('the user page shows the 1 MB quota set at creation', (await page.locator('[data-testid="user-usage"]').innerText()).includes('1.0 MB'));
  await page.locator('#e-quota').fill('1');
  await page.getByRole('button', { name: 'Save' }).first().click();
  await page.waitForURL(/\?done=saved/);
  check('saving the quota confirms', (await page.getByRole('status').first().innerText()).includes('Saved'));
  check('a small message is delivered', deliver(tiny.email, message(tiny.email, `small-${RUN}`, 8)));
  const usageShown = await eventually(async () => {
    await page.reload();
    return !(await page.locator('[data-testid="user-usage"]').innerText()).startsWith('0 B');
  }, 60);
  check('the usage figure appears on the user page', usageShown, await page.locator('[data-testid="user-usage"]').innerText());
  check('a 2 MB message is accepted by Postfix (the quota is Dovecot\'s job)', deliver(tiny.email, message(tiny.email, `toobig-${RUN}`, 2048)));
  const refused = await eventually(() => {
    // Only the last few minutes: the whole log of a long-lived dev container exceeds spawnSync's buffer.
    const logs = run('docker', ['logs', '--since', '10m', 'rustmail-mta'], { allowFail: true, maxBuffer: 64 * 1024 * 1024 });
    return new RegExp(`${tiny.email.replace('.', '\\.')}.*(Quota exceeded|552)`, 'i').test(`${logs.stdout}\n${logs.stderr}`);
  }, 90);
  check('Dovecot refused the over-quota message', refused);

  await page.goto(server.url + '/admin/quota');
  const charts = await eventually(async () => Number(await page.locator('[x-data="quotaCharts"]').getAttribute('data-charts-ready')) >= 1, 20);
  check('the quota charts are drawn (Chart.js loaded locally)', charts);
  check('no chart-failed warning', !(await page.getByText('The charts could not be loaded').isVisible()));
  const topRow = page.locator('[data-testid="quota-top"] tr', { hasText: tiny.email });
  check('the dashboard lists the mailbox with its usage and quota', (await topRow.count()) === 1 && (await topRow.innerText()).includes('1.0 MB'), await topRow.allInnerTexts().then((t) => t.join('|')));
  const canvas = await page.locator('#chart-domains').boundingBox();
  check('the chart has a real size', !!canvas && canvas.width > 100 && canvas.height > 100);
  await page.screenshot({ path: `${SHOTS}/02-quota.png`, fullPage: true });

  // ── domain administrator ──────────────────────────────────────────────────
  section('Domain administrator stays inside their domain');
  {
    const c = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US' });
    const p = await c.newPage();
    const pp = watch(p);
    await login(p, server.url, `dana@${HOME_DOMAIN}`);
    await p.goto(server.url + '/admin/users');
    const text = await p.locator('main').innerText();
    check('dana sees her own domain\'s users', text.includes(`root@${HOME_DOMAIN}`));
    check('…and nobody else\'s', !text.includes(OTHER_DOMAIN) && !text.includes(NEW_DOMAIN));
    const nav = await p.locator('nav[aria-label]').first().innerText();
    check('the side menu has no Health entry', !nav.includes('Health'));
    await p.goto(server.url + '/admin/domains');
    check('there is no "new domain" form for her', (await p.locator('#d-name').count()) === 0);
    const zed = psql(`SELECT id FROM users WHERE email = 'zed@${OTHER_DOMAIN}'`).stdout.trim();
    const r = await p.goto(`${server.url}/admin/users/${zed}`);
    check('another tenant\'s user page is refused', r.status() === 403 && (await p.getByText('Access denied').first().isVisible()), String(r.status()));
    const h = await p.goto(server.url + '/admin/health');
    check('system health is refused', h.status() === 403);
    await p.goto(server.url + '/admin/users/new');
    const roles = await p.locator('#n-role option').allInnerTexts();
    check('she can only create ordinary users', roles.length === 1 && roles[0] === 'User', roles.join('|'));
    await p.goto(server.url + '/admin/audit');
    const audit = await p.locator('main').innerText();
    check('her audit log does not contain the other tenants\' actions', !audit.includes(NEW_DOMAIN));
    check('dana: no console errors', pp.length === 0, pp.join(' | '));
    await c.close();
  }

  // ── CSV import ────────────────────────────────────────────────────────────
  section('CSV import');
  await page.goto(server.url + '/admin/import');
  const bad = `email,name,role,quota_mb,password\nbroken-address,Nobody,user,,\nimp1@${NEW_DOMAIN},Imp One,user,5,\n`;
  await page.locator('#i-csv').fill(bad);
  await submit(page, '/admin/import/preview', page.getByRole('button', { name: 'Check' }));
  await page.locator('[data-testid="import-problems"]').waitFor();
  check('a bad row is reported with its line', (await page.locator('[data-testid="import-problems"]').innerText()).includes('broken-address'));
  check('nothing can be applied while there are problems', (await page.getByRole('button', { name: /Create \d+ users/ }).count()) === 0);
  const good = `email,name,role,quota_mb,password\nimp1@${NEW_DOMAIN},Imp One,user,5,\nimp2@${NEW_DOMAIN},Imp Two,user,,imported-password-9\n`;
  await page.locator('#i-csv').fill(good);
  await submit(page, '/admin/import/preview', page.getByRole('button', { name: 'Check' }));
  await page.locator('[data-testid="import-valid"]').waitFor();
  check('a clean file shows a preview with an apply button', (await page.getByRole('button', { name: 'Create 2 users' }).count()) === 1);
  check('the preview created nothing', psql(`SELECT count(*) FROM users WHERE email LIKE 'imp%@${NEW_DOMAIN}'`).stdout.trim() === '0');
  await submit(page, '/admin/import/apply', page.getByRole('button', { name: 'Create 2 users' }));
  await page.locator('[data-testid="import-result-summary"]').waitFor();
  check('both users were created', (await page.locator('[data-testid="import-result-summary"]').innerText()).includes('2 created'));
  const html = await page.content();
  check('the generated password is not in the page', !/imported-password-9/.test(html));
  const [download] = await Promise.all([page.waitForEvent('download'), page.locator('[data-testid="import-download"]').click()]);
  const file = readFileSync(await download.path(), 'utf8');
  // Columns: line,email,result,password,error
  const fields = (email) => (file.split('\n').find((l) => l.includes(`,${email},`)) || '').split(',');
  const imp1pw = fields(`imp1@${NEW_DOMAIN}`)[3] || '';
  check('the downloaded file has the generated password for imp1', imp1pw.length >= 12, file.slice(0, 200));
  check('…and none for imp2, whose password was given', (fields(`imp2@${NEW_DOMAIN}`)[3] || '') === '');
  const link = await page.locator('[data-testid="import-download"]').getAttribute('href');
  const again = await page.request.get(server.url + link);
  check('the file cannot be downloaded a second time', again.status() === 404, String(again.status()));
  for (const [email, password] of [[`imp1@${NEW_DOMAIN}`, imp1pw], [`imp2@${NEW_DOMAIN}`, 'imported-password-9']]) {
    const c = await browser.newContext({ locale: 'en-US' });
    const p = await c.newPage();
    let ok = true;
    try { await login(p, server.url, email, password); } catch { ok = false; }
    check(`imported ${email} signs in`, ok);
    await c.close();
  }

  // ── audit ─────────────────────────────────────────────────────────────────
  section('Audit log');
  await page.goto(server.url + '/admin/audit');
  await page.locator('#au-action').selectOption('user.create');
  await page.locator('#au-text').fill(String(RUN)); // only this run's entries, not leftovers of earlier runs
  await page.getByRole('button', { name: 'Filter' }).click();
  await page.waitForURL(/action=user\.create/);
  const rows = await page.locator('[data-testid="audit-table"] tbody tr').allInnerTexts();
  check('filtering by action lists the creations made above', rows.length >= 3 && rows.every((r) => r.includes('user.create')), String(rows.length));
  check('each row names the administrator', rows.every((r) => r.includes(`root@${HOME_DOMAIN}`)));
  await page.locator('#au-text').fill('no-such-text-' + RUN);
  await page.getByRole('button', { name: 'Filter' }).click();
  await page.waitForURL(/text=no-such-text/);
  check('a filter that matches nothing says so', await page.getByText('No entries match.').isVisible());
  await page.goto(server.url + '/admin/audit?action=user.create');
  const [csv] = await Promise.all([page.waitForEvent('download'), page.getByRole('link', { name: 'Export CSV' }).click()]);
  const csvText = readFileSync(await csv.path(), 'utf8');
  check('the CSV export honours the filter', csvText.includes('user.create') && !csvText.includes('domain.create'), csvText.slice(0, 150));

  // ── health ────────────────────────────────────────────────────────────────
  section('System health');
  await page.goto(server.url + '/admin/health');
  const table = await page.locator('[data-testid="health-table"]').innerText();
  check('the database check passes', /Database[\s\S]*OK/.test(table));
  check('IMAP and SMTP are probed', /IMAP/.test(table) && /SMTP/.test(table));
  check('what cannot be probed here is "Not checked", not "Down"', /Not checked/.test(table));
  check('an overall status is shown', (await page.locator('[data-testid="health-overall"]').innerText()).length > 0);
  await page.screenshot({ path: `${SHOTS}/03-health.png`, fullPage: true });

  // ── deleting ──────────────────────────────────────────────────────────────
  section('Deleting');
  const plain = users[2];
  await page.goto(server.url + '/admin/users?q=plain');
  await page.getByRole('link', { name: plain.email }).click();
  await page.locator('#x-confirm').fill('wrong@address');
  page.once('dialog', (d) => d.accept());
  await submit(page, '/delete', page.getByRole('button', { name: 'Delete' }));
  check('a wrong confirmation is refused', await page.locator('#delete-status .alert-danger').isVisible());
  check('…and the Maildir is still there', existsSync(path.join(VMAIL, NEW_DOMAIN, 'plain')));
  await page.locator('#x-confirm').fill(plain.email);
  page.once('dialog', (d) => d.accept());
  await submit(page, '/delete', page.getByRole('button', { name: 'Delete' }));
  await page.waitForURL(/\/admin\/users\?done=deleted/);
  check('typing the address deletes the user', psql(`SELECT count(*) FROM users WHERE email = '${plain.email}'`).stdout.trim() === '0');
  check('…and removes the Maildir', !existsSync(path.join(VMAIL, NEW_DOMAIN, 'plain')));
  await page.goto(domainUrl);
  await page.locator('#d-confirm').fill(NEW_DOMAIN);
  page.once('dialog', (d) => d.accept());
  await submit(page, '/delete', page.getByRole('button', { name: 'Delete' }));
  check('a domain that still has users cannot be deleted', await page.locator('#domain-delete-status .alert-danger').isVisible());
  const myself = psql(`SELECT id FROM users WHERE email = 'root@${HOME_DOMAIN}'`).stdout.trim();
  await page.goto(`${server.url}/admin/users/${myself}`);
  check('there is no delete or disable control for yourself', (await page.locator('#x-confirm').count()) === 0 && (await page.locator('#s-status').count()) === 0);

  check('no console or CSP errors anywhere on the console pages', problems.length === 0, problems.join(' | '));
  await rootCtx.close();
} catch (err) {
  console.log(`\nSCRIPT ERROR: ${err.stack || err}`);
  failures.push(`script error: ${err.message}`);
} finally {
  await browser.close();
  server.stop();
  // Clean up: users first (they reference the domain), then the domains and their directories.
  for (const d of createdDomains) {
    psql(`DELETE FROM quota_usage WHERE username LIKE '%@${d}'`);
    psql(`DELETE FROM users WHERE email LIKE '%@${d}'`);
    psql(`DELETE FROM domains WHERE name = '${d}'`);
    rmSync(path.join(VMAIL, d), { recursive: true, force: true });
  }
  psql(`DELETE FROM audit_log WHERE details::text LIKE '%${RUN}%'`);
}
console.log(`\n${pass} passed, ${failures.length} failed`);
if (failures.length) {
  for (const f of failures) console.log(`  FAILED: ${f}`);
  console.log(`\nServer log:\n${server.log().slice(-3000)}`);
  process.exit(1);
}
