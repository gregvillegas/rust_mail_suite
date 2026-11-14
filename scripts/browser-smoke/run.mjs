// Browser smoke test for the RustMail web UI. Drives a real Chrome against the real
// server and database; it covers what the Rust HTTP tests cannot (Alpine, htmx,
// theme, dialog, CSP, CDN fallback). Optional: not run by CI.
//
//   cd scripts/browser-smoke && npm install && npm test
//
// Needs: the dev Postgres from `make dev`, `cargo build -p rustmail-web -p rustmail-cli`,
// psql on PATH, and Chrome (set CHROME_PATH if it is not in the macOS default location).
// The CDN scenarios need internet access. It creates and drops its own database.
import { chromium } from 'playwright-core';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, writeFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import os from 'node:os';
import net from 'node:net';
import crypto from 'node:crypto';

const R = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const WEB = `${R}/target/debug/rustmail-web`;
const CLI = `${R}/target/debug/rustmail-cli`;
const BASE = process.env.E2E_DB_BASE || 'postgres://rustmail:rustmail@localhost:5433';
const PGPORT = new URL(BASE).port || '5432';
const PGUSER = decodeURIComponent(new URL(BASE).username);
const PGPASS = decodeURIComponent(new URL(BASE).password);
const CHROME = process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const DB = 'rustmail_e2e';
const PW = 'e2e-password-12345';
const SHOTS = process.env.E2E_SHOTS || path.join(os.tmpdir(), 'rustmail-browser-smoke');
mkdirSync(SHOTS, { recursive: true });

const servers = [];
let pass = 0;
const failures = [];
function check(label, ok, detail = '') {
  if (ok) pass++; else failures.push(`${label} ${detail}`);
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${label}${ok || !detail ? '' : '  -> ' + detail}`);
}
const section = (t) => console.log(`\n${t}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** Waits (up to 3 s) for a locator to reach the given visibility; returns whether it did. */
async function becomes(locator, state) {
  try { await locator.waitFor({ state, timeout: 3000 }); return true; } catch { return false; }
}

function psql(db, sql) {
  const r = spawnSync('psql', ['-h', 'localhost', '-p', PGPORT, '-U', PGUSER, '-q', '-d', db, '-c', sql],
    { env: { ...process.env, PGPASSWORD: PGPASS }, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`psql failed: ${r.stderr}`);
}
function cli(args, env = {}) {
  const r = spawnSync(CLI, ['--database-url', `${BASE}/${DB}`, ...args],
    { cwd: os.tmpdir(), env: { ...process.env, ...env }, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`cli ${args.join(' ')} failed: ${r.stderr}`);
}
function freePort() {
  return new Promise((res) => { const s = net.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); });
}

async function startServer(extra = {}) {
  const port = await freePort();
  const proc = spawn(WEB, [], {
    cwd: os.tmpdir(),
    env: {
      PATH: process.env.PATH, DATABASE_URL: `${BASE}/${DB}`, JWT_SECRET: crypto.randomBytes(32).toString('hex'),
      WEB_BIND_ADDR: `127.0.0.1:${port}`, COOKIE_SECURE: 'false', RUST_LOG: 'warn',
      LOGIN_RATE_BURST: '100', LOGIN_RATE_PERIOD_SECS: '1', ASSET_SOURCE: 'local', ...extra,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  proc.stdout.on('data', (d) => (log += d)); proc.stderr.on('data', (d) => (log += d));
  const url = `http://127.0.0.1:${port}`;
  servers.push(proc);
  for (let i = 0; i < 60; i++) {
    try { if ((await fetch(url + '/healthz')).ok) break; } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 250));
  }
  return { url, stop: () => proc.kill('SIGINT'), log: () => log };
}

/** Collects console errors (ignoring the browser's own "Failed to load resource" for expected 4xx). */
function watch(page) {
  const problems = [];
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    const t = m.text();
    if (/Failed to load resource/.test(t)) return;
    problems.push(t);
  });
  page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`));
  return problems;
}

async function login(page, base, email, password = PW) {
  await page.goto(base + '/login');
  await page.getByLabel('Email address').fill(email);
  await page.locator('#login-password').fill(password);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
}

const browser = await chromium.launch({
  executablePath: CHROME, headless: true,
});

try {
  // ── database + users ────────────────────────────────────────────────────
  psql('postgres', `DROP DATABASE IF EXISTS ${DB} WITH (FORCE)`); psql('postgres', `CREATE DATABASE ${DB}`);
  cli(['migrate']);
  for (const [local, name] of [['root', 'Root'], ['dana', 'Dana'], ['alice', 'Alice']]) {
    cli(['seed', '--domain', 'example.com', '--admin-local-part', local, '--admin-name', name], { RUSTMAIL_ADMIN_PASSWORD: PW });
  }
  psql(DB, `UPDATE users SET role='domain_admin' WHERE local_part='dana'`);
  psql(DB, `UPDATE users SET role='user' WHERE local_part='alice'`);

  // ═══ A. local assets ═══════════════════════════════════════════════════
  const A = await startServer();
  section(`A. local assets  (${A.url})`);
  {
    const ctx = await browser.newContext({ viewport: { width: 1100, height: 800 }, colorScheme: 'light', locale: 'en-US' });
    const page = await ctx.newPage();
    const problems = watch(page);

    await page.goto(A.url + '/login');
    check('login page title', (await page.title()) === 'Sign in · RustMail', await page.title());
    check('Alpine started (x-data initialised)', await page.evaluate(() => !!window.Alpine && typeof window.Alpine.version === 'string'));
    check('htmx loaded', await page.evaluate(() => typeof window.htmx !== 'undefined'));
    await page.screenshot({ path: `${SHOTS}/01-login-light.png` });

    // Skip link is the first tab stop and becomes visible on focus.
    await page.keyboard.press('Tab');
    const skip = await page.evaluate(() => { const a = document.activeElement; const r = a.getBoundingClientRect(); return { text: a.textContent.trim(), w: r.width, h: r.height }; });
    check('first Tab stop is the skip link, and it is visible', skip.text === 'Skip to main content' && skip.w > 20 && skip.h > 10, JSON.stringify(skip));

    // Theme.
    await page.getByRole('button', { name: 'Theme' }).click();
    check('theme menu opens', await becomes(page.getByRole('button', { name: 'Dark' }), 'visible'));
    await page.getByRole('button', { name: 'Dark' }).click();
    check('choosing Dark sets data-bs-theme', (await page.getAttribute('html', 'data-bs-theme')) === 'dark');
    check('…and persists the choice', (await page.evaluate(() => localStorage.getItem('rm-theme'))) === 'dark');
    check('menu closes after choosing', await becomes(page.getByRole('button', { name: 'Light' }), 'hidden'));
    await page.screenshot({ path: `${SHOTS}/02-login-dark.png` });
    await page.reload();
    check('dark theme survives a reload', (await page.getAttribute('html', 'data-bs-theme')) === 'dark');
    await page.getByRole('button', { name: 'Theme' }).click();
    await page.getByRole('button', { name: 'Auto' }).click();
    check('Auto clears the stored choice', (await page.evaluate(() => localStorage.getItem('rm-theme'))) === null);
    await page.emulateMedia({ colorScheme: 'dark' });
    check('Auto follows the OS live (dark)', await page.waitForFunction(() => document.documentElement.getAttribute('data-bs-theme') === 'dark', null, { timeout: 3000 }).then(() => true, () => false));
    await page.emulateMedia({ colorScheme: 'light' });
    check('Auto follows the OS live (light)', await page.waitForFunction(() => document.documentElement.getAttribute('data-bs-theme') === 'light', null, { timeout: 3000 }).then(() => true, () => false));
    // Escape closes an open menu.
    await page.getByRole('button', { name: 'Theme' }).click();
    await page.keyboard.press('Escape');
    check('Escape closes the menu', await becomes(page.getByRole('button', { name: 'Light' }), 'hidden'));
    // Clicking elsewhere closes it too.
    await page.getByRole('button', { name: 'Theme' }).click();
    await page.getByRole('heading', { name: 'Sign in' }).click();
    check('clicking outside closes the menu', await becomes(page.getByRole('button', { name: 'Light' }), 'hidden'));

    // Password reveal.
    await page.locator('#login-password').fill('secret');
    const toggle = page.locator('button[aria-pressed]');
    check('password hidden by default', (await page.getAttribute('#login-password', 'type')) === 'password');
    await toggle.click();
    check('toggle reveals the password', (await page.getAttribute('#login-password', 'type')) === 'text');
    check('…and the button label follows', (await toggle.getAttribute('aria-label')) === 'Hide password');
    await toggle.click();
    check('toggle hides it again', (await page.getAttribute('#login-password', 'type')) === 'password');

    // Wrong password: inline error via htmx, no page reload.
    await page.evaluate(() => { window.__marker = 'same-document'; });
    await page.getByLabel('Email address').fill('alice@example.com');
    await page.locator('#login-password').fill('wrong-password-xyz');
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page.locator('#login-error').waitFor();
    check('wrong password shows an inline error', (await page.locator('#login-error').textContent()).includes('Invalid email or password.'));
    check('…without reloading the document', (await page.evaluate(() => window.__marker)) === 'same-document');
    check('…focus moves to the error for screen readers', await page.evaluate(() => document.activeElement && document.activeElement.id === 'login-error'));
    check('…the address is kept, the password is cleared', (await page.inputValue('#login-email')) === 'alice@example.com' && (await page.inputValue('#login-password')) === '');
    check('…and the form is usable again', await page.getByRole('button', { name: 'Sign in', exact: true }).isEnabled());
    await page.screenshot({ path: `${SHOTS}/03-login-error.png` });

    // Language switch.
    await page.getByRole('button', { name: 'Language' }).click();
    await Promise.all([page.waitForLoadState('load'), page.getByRole('button', { name: 'Deutsch' }).click()]);
    await page.getByRole('heading', { name: 'Anmelden' }).waitFor();
    check('switching language re-renders the page', (await page.getAttribute('html', 'lang')) === 'de' && await page.getByLabel('E-Mail-Adresse').isVisible());
    check('…and stores a cookie', (await ctx.cookies()).some((c) => c.name === 'rm_lang' && c.value === 'de'));
    await page.getByRole('button', { name: 'Sprache' }).click();
    await Promise.all([page.waitForLoadState('load'), page.getByRole('button', { name: 'English' }).click()]);
    await page.getByRole('heading', { name: 'Sign in' }).waitFor();

    // Successful sign-in: full navigation to the dashboard, activity loads via htmx.
    await page.getByLabel('Email address').fill('alice@example.com');
    await page.locator('#login-password').fill(PW);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page.waitForURL(A.url + '/');
    await page.getByRole('heading', { name: /Welcome back, Alice/ }).waitFor();
    check('sign-in lands on the dashboard', true);
    await page.locator('table.rm-activity').waitFor();
    const rows = await page.locator('table.rm-activity tbody tr').count();
    check('recent activity loaded through htmx (failed + successful)', rows === 2, `rows=${rows}`);
    check('…with success and failure badges', (await page.locator('.badge.text-bg-danger').count()) === 1 && (await page.locator('.badge.text-bg-success').count()) === 1);
    check('plain users have no Administration link', (await page.getByRole('link', { name: 'Administration' }).count()) === 0);
    const csrfCookie = (await ctx.cookies()).find((c) => c.name === 'rm_csrf');
    check('CSRF cookie is script-readable; access/refresh are HttpOnly',
      csrfCookie && !csrfCookie.httpOnly && (await ctx.cookies()).filter((c) => c.name !== 'rm_csrf' && c.name !== 'rm_lang').every((c) => c.httpOnly));
    check('script cannot read the session tokens', await page.evaluate(() => !/rm_access|rm_refresh/.test(document.cookie)));
    await page.screenshot({ path: `${SHOTS}/04-dashboard.png`, fullPage: true });

    // Sign-out-everywhere dialog.
    const trigger = page.getByRole('button', { name: 'Sign out of all devices' });
    const dialog = page.getByRole('dialog');
    check('dialog is hidden initially', !(await dialog.isVisible()));
    await trigger.click();
    check('dialog opens', await becomes(dialog, 'visible'));
    check('…labelled for assistive tech', (await dialog.getAttribute('aria-modal')) === 'true' && (await dialog.getAttribute('aria-labelledby')) === 'logout-all-title');
    check('…focus starts on Cancel', await page.evaluate(() => document.activeElement && document.activeElement.textContent.trim() === 'Cancel'));
    await page.screenshot({ path: `${SHOTS}/05-dialog.png` });
    await page.keyboard.press('Escape');
    check('Escape closes the dialog', await becomes(dialog, 'hidden'));
    check('…and returns focus to the trigger', await page.evaluate(() => document.activeElement && document.activeElement.textContent.trim() === 'Sign out of all devices'));
    await trigger.click();
    // Alpine shows x-show content on a setTimeout(0) so the opening click cannot
    // also count as a click-outside; wait for it before clicking the backdrop.
    await becomes(dialog, 'visible');
    await page.mouse.click(5, 5); // the backdrop
    check('clicking the backdrop closes it', await becomes(dialog, 'hidden'));
    await trigger.click();
    await page.getByRole('button', { name: 'Cancel' }).click();
    check('Cancel closes it without signing out', (await becomes(dialog, 'hidden')) && page.url() === A.url + '/');

    // Open a second session for alice, then sign out everywhere from the first.
    const ctx2 = await browser.newContext({ locale: 'en-US' });
    const page2 = await ctx2.newPage();
    await login(page2, A.url, 'alice@example.com');
    await page2.waitForURL(A.url + '/');

    await trigger.click();
    await page.getByRole('button', { name: 'Sign out everywhere' }).click();
    await page.waitForURL(/\/login/);
    check('sign out everywhere leads to the sign-in page', true);
    check('…and clears the cookies', !(await ctx.cookies()).some((c) => c.name === 'rm_access' && c.value));
    await page2.reload();
    await page2.waitForURL(/\/(login|session\/renew)/);
    await page2.waitForURL(/\/login/, { timeout: 15000 });
    check('…and the other device is signed out too', /\/login/.test(page2.url()), page2.url());
    await ctx2.close();

    // Account menu sign-out.
    await login(page, A.url, 'alice@example.com');
    await page.waitForURL(A.url + '/');
    await page.getByRole('button', { name: 'Account menu' }).click();
    check('account menu shows who is signed in', await page.getByText('Signed in as alice@example.com').isVisible());
    await page.getByRole('button', { name: 'Sign out', exact: true }).click();
    await page.waitForURL(/\/login/);
    check('Sign out returns to the sign-in page', true);
    await page.goto(A.url + '/');
    check('…and protected pages now redirect to sign in', /\/login$/.test(page.url()), page.url());

    // Role gating in the browser.
    await login(page, A.url, 'alice@example.com');
    await page.waitForURL(A.url + '/');
    const denied = await page.goto(A.url + '/admin');
    check('plain user opening /admin gets a 403 page', denied.status() === 403 && (await page.getByRole('heading', { name: 'Access denied' }).isVisible()));
    await page.screenshot({ path: `${SHOTS}/06-forbidden.png` });
    const missing = await page.goto(A.url + '/nowhere');
    check('unknown URL gets a styled 404 page', missing.status() === 404 && (await page.getByRole('heading', { name: 'Page not found' }).isVisible()));
    await page.getByRole('button', { name: 'Account menu' }).click();
    await page.getByRole('button', { name: 'Sign out', exact: true }).click();
    await page.waitForURL(/\/login/);

    // Admin: boosted navigation without a full reload, then the polled partial.
    await login(page, A.url, 'root@example.com');
    await page.waitForURL(A.url + '/');
    await page.evaluate(() => { window.__marker = 'same-document'; });
    await page.getByRole('link', { name: 'Administration' }).first().click();
    await page.waitForURL(A.url + '/admin');
    check('boosted navigation swaps the page without a reload', (await page.evaluate(() => window.__marker)) === 'same-document');
    check('…and updates the title', (await page.title()) === 'Administration · RustMail');
    await page.locator('#admin-overview').getByText(/Connected \(\d+ ms\)/).waitFor();
    check('admin overview loads via htmx', true);
    check('super admin sees the domain count', await page.locator('#admin-overview').getByText('Domains').isVisible());
    check('active nav item is marked', (await page.locator('.rm-sidebar a.active').getAttribute('aria-current')) === 'page');
    await page.screenshot({ path: `${SHOTS}/07-admin-dark.png`.replace('dark', 'light') });
    await page.goBack();
    await page.getByRole('heading', { name: /Welcome back, Root/ }).waitFor();
    check('browser Back works', page.url() === A.url + '/');
    await page.getByRole('link', { name: 'Administration' }).first().click();
    await page.waitForURL(A.url + '/admin');

    // Domain admin must not see server-wide numbers.
    const ctx3 = await browser.newContext({ locale: 'en-US' });
    const page3 = await ctx3.newPage();
    await login(page3, A.url, 'dana@example.com');
    await page3.waitForURL(A.url + '/');
    await page3.getByRole('link', { name: 'Administration' }).first().click();
    await page3.locator('#admin-overview').getByText(/Connected/).waitFor();
    check('domain admin sees the page but not the server-wide domain count', (await page3.locator('#admin-overview').getByText('Domains').count()) === 0);
    await ctx3.close();

    // Toasts: CSRF, network, and dismissal.
    const ctx4 = await browser.newContext({ locale: 'en-US' });
    const page4 = await ctx4.newPage();
    await page4.goto(A.url + '/login');
    await ctx4.clearCookies({ name: 'rm_csrf' });
    await page4.getByLabel('Email address').fill('alice@example.com');
    await page4.locator('#login-password').fill(PW);
    await page4.getByRole('button', { name: 'Sign in', exact: true }).click();
    const toast = page4.locator('#rm-toasts .alert');
    await toast.waitFor();
    check('a missing CSRF token shows a toast, not raw JSON', (await toast.textContent()).includes('Your page is out of date.') && !(await page4.content()).includes('csrf_failed'));
    check('…the form is still on the page', await page4.locator('#login-form').isVisible());
    await toast.getByRole('button', { name: 'Close' }).click();
    check('toasts can be dismissed', (await page4.locator('#rm-toasts .alert').count()) === 0);
    await page4.reload();
    await ctx4.setOffline(true);
    await page4.getByLabel('Email address').fill('alice@example.com');
    await page4.locator('#login-password').fill(PW);
    await page4.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page4.locator('#rm-toasts .alert').waitFor();
    check('a network failure shows a toast', (await page4.locator('#rm-toasts .alert').textContent()).includes('Network error.'));
    await ctx4.setOffline(false);
    await ctx4.close();

    // Mobile layout.
    const mobile = await browser.newContext({ viewport: { width: 375, height: 700 }, locale: 'en-US' });
    const mp = await mobile.newPage();
    await mp.goto(A.url + '/login');
    check('mobile: menu collapsed by default', !(await mp.getByRole('button', { name: 'Theme' }).isVisible()));
    await mp.getByRole('button', { name: 'Toggle navigation' }).click();
    check('mobile: toggler opens the menu', (await becomes(mp.getByRole('button', { name: 'Theme' }), 'visible')) && (await mp.getByRole('button', { name: 'Toggle navigation' }).getAttribute('aria-expanded')) === 'true');
    await mp.screenshot({ path: `${SHOTS}/08-mobile-menu.png` });
    await mp.getByRole('button', { name: 'Toggle navigation' }).click();
    check('mobile: toggler closes it again', await becomes(mp.getByRole('button', { name: 'Theme' }), 'hidden'));
    const overflow = await mp.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    check('mobile: no horizontal scrolling', overflow <= 0, `overflow=${overflow}px`);
    await mobile.close();

    check('no console errors or CSP violations on any page above', problems.length === 0, JSON.stringify(problems));
    await ctx.close();
  }
  A.stop();

  // ═══ B. silent session renewal ═════════════════════════════════════════
  const B = await startServer({ JWT_ACCESS_TTL_SECS: '3' });
  section(`B. silent session renewal, 3 s access tokens  (${B.url})`);
  {
    const ctx = await browser.newContext({ locale: 'en-US' });
    const page = await ctx.newPage();
    const problems = watch(page);
    const visited = [];
    page.on('framenavigated', (f) => { if (f === page.mainFrame()) visited.push(new URL(f.url()).pathname); });

    await login(page, B.url, 'alice@example.com');
    await page.waitForURL(B.url + '/');
    await new Promise((r) => setTimeout(r, 4500)); // let the access token lapse
    visited.length = 0;
    await page.goto(B.url + '/');
    await page.getByRole('heading', { name: /Welcome back, Alice/ }).waitFor();
    check('an expired access token is renewed transparently', visited.includes('/session/renew') && visited.at(-1) === '/', JSON.stringify(visited));

    // Same, but the refresh cookie is gone: end up at sign-in, remembering the page.
    await new Promise((r) => setTimeout(r, 4500));
    await ctx.clearCookies({ name: 'rm_refresh' });
    await page.goto(B.url + '/admin');
    await page.waitForURL(/\/login/, { timeout: 15000 });
    check('renewal that cannot succeed ends at sign-in, remembering the page', page.url().endsWith('/login?next=%2Fadmin'), page.url());

    // After signing in again the user is taken to the page they wanted.
    await page.getByLabel('Email address').fill('root@example.com');
    await page.locator('#login-password').fill(PW);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page.waitForURL(B.url + '/admin');
    check('…and signing in returns to it', true);
    check('no console errors', problems.length === 0, JSON.stringify(problems));
    await ctx.close();
  }
  B.stop();

  // ═══ C. rate limiting ══════════════════════════════════════════════════
  const C = await startServer({ LOGIN_RATE_BURST: '3', LOGIN_RATE_PERIOD_SECS: '3600' });
  section(`C. login rate limit, burst 3  (${C.url})`);
  {
    const ctx = await browser.newContext({ locale: 'en-US' });
    const page = await ctx.newPage();
    await page.goto(C.url + '/login');
    for (let i = 0; i < 3; i++) {
      await page.getByLabel('Email address').fill('alice@example.com');
      await page.locator('#login-password').fill('wrong-password-xyz');
      await page.getByRole('button', { name: 'Sign in', exact: true }).click();
      await page.locator('#login-error').waitFor();
      await page.evaluate(() => { const e = document.getElementById('login-error'); if (e) e.removeAttribute('id'); }); // so the next wait is meaningful
    }
    await page.getByLabel('Email address').fill('alice@example.com');
    await page.locator('#login-password').fill(PW);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page.locator('#rm-toasts .alert').waitFor();
    check('the fourth attempt is stopped and explained in a toast', (await page.locator('#rm-toasts .alert').textContent()).includes('Too many attempts.'));
    check('…even with the right password, and the user stays on the form', page.url().endsWith('/login') && (await page.locator('#login-form').isVisible()));
    await ctx.close();
  }
  C.stop();

  // ═══ D. CDN mode: real CDN, SRI, and the local fallback ════════════════
  const D = await startServer({ ASSET_SOURCE: 'cdn' });
  section(`D. CDN mode  (${D.url})`);
  {
    const cdnProbe = async (route) => {
      const ctx = await browser.newContext({ locale: 'en-US' });
      const page = await ctx.newPage();
      const problems = watch(page);
      const cdn = [], local = [];
      page.on('response', (r) => {
        const u = r.url();
        if (u.includes('cdn.jsdelivr.net')) cdn.push(`${r.status()} ${u.split('/').slice(-1)[0]}`);
        if (u.includes('/static/vendor/')) local.push(`${r.status()} ${u.split('/').slice(-1)[0].split('?')[0]}`);
      });
      if (route) await route(ctx);
      await page.goto(D.url + '/login', { waitUntil: 'load' });
      await page.waitForTimeout(800); // allow the fallback script to run
      const state = await page.evaluate(() => ({
        htmx: typeof window.htmx !== 'undefined', alpine: !!window.Alpine,
        css: getComputedStyle(document.querySelector('.btn-primary')).backgroundColor,
        evil: typeof window.__evil !== 'undefined',
      }));
      return { ctx, page, problems, cdn, local, state };
    };

    // (a) the real CDN, with real integrity hashes.
    const a = await cdnProbe();
    check('all three libraries came from jsDelivr with HTTP 200', a.cdn.length === 3 && a.cdn.every((l) => l.startsWith('200')), JSON.stringify(a.cdn));
    check('…and passed their Subresource Integrity checks (no console errors)', a.problems.filter((p) => /integrity|Subresource/i.test(p)).length === 0, JSON.stringify(a.problems));
    check('…so the local fallback was NOT needed', a.local.length === 0, JSON.stringify(a.local));
    check('htmx, Alpine and Bootstrap CSS are all active', a.state.htmx && a.state.alpine && a.state.css !== 'rgba(0, 0, 0, 0)', JSON.stringify(a.state));
    await a.page.getByRole('button', { name: 'Theme' }).click();
    check('the UI works from CDN copies (theme menu)', await becomes(a.page.getByRole('button', { name: 'Dark' }), 'visible'));
    await a.ctx.close();

    // (b) CDN unreachable.
    const b = await cdnProbe((ctx) => ctx.route('**://cdn.jsdelivr.net/**', (r) => r.abort()));
    check('CDN unreachable: the vendored copies are loaded instead', b.local.length === 3 && b.local.every((l) => l.startsWith('200')), JSON.stringify(b.local));
    check('…and everything still works', b.state.htmx && b.state.alpine && b.state.css !== 'rgba(0, 0, 0, 0)', JSON.stringify(b.state));
    await b.page.getByRole('button', { name: 'Theme' }).click();
    check('…including Alpine components started late', await becomes(b.page.getByRole('button', { name: 'Dark' }), 'visible'));
    await b.page.screenshot({ path: `${SHOTS}/09-cdn-blocked-fallback.png` });
    await b.page.keyboard.press('Escape');
    await b.page.getByLabel('Email address').fill('alice@example.com');
    await b.page.locator('#login-password').fill(PW);
    await b.page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await b.page.waitForURL(D.url + '/');
    check('…and signing in works end to end on the fallback copies', true);
    await b.ctx.close();

    // (c) a tampered CDN response must be refused by SRI, then replaced by the local copy.
    const c = await cdnProbe((ctx) => ctx.route('**://cdn.jsdelivr.net/**', async (route) => {
      const u = route.request().url();
      const real = await route.fetch();
      const body = await real.text();
      if (u.endsWith('htmx.min.js')) return route.fulfill({ response: real, body: body + '\nwindow.__evil=1;' });
      if (u.endsWith('bootstrap.min.css')) return route.fulfill({ response: real, body: body + '\nbody{display:none!important}' });
      return route.fulfill({ response: real, body });
    }));
    check('tampered htmx is rejected by integrity checking', !c.state.evil);
    check('…and replaced by the vendored copy', c.local.some((l) => l.includes('htmx.min.js')) && c.state.htmx, JSON.stringify(c.local));
    const visible = await c.page.evaluate(() => getComputedStyle(document.body).display);
    check('tampered Bootstrap CSS is rejected too (page not hidden)', visible !== 'none', visible);
    await c.ctx.close();
  }
  D.stop();

  // ═══ E. quarantine (Phase 5) ══════════════════════════════════════════
  {
    const qdir = mkdtempSync(path.join(os.tmpdir(), 'rm-quarantine-'));
    const MSG = 'Return-Path: <x@example.org>\nDelivered-To: spam-quarantine\nSubject: Win big\n\n<b>hello</b> body text\n';
    writeFileSync(path.join(qdir, 'spam-e2e1'), MSG);
    writeFileSync(path.join(qdir, 'virus-e2e2'), MSG);
    psql(DB, `INSERT INTO quarantine_items (mail_id, recipient, sender, kind, reason, file_name, received_at) VALUES
      ('e2e1','alice@example.com','x@example.org','spam','score 14.2','spam-e2e1', now()),
      ('e2e2','alice@example.com','x@example.org','virus','Eicar-Test','virus-e2e2', now() - interval '1 minute')`);
    // A tiny SMTP sink standing in for Postfix's re-entry port.
    let smtpSeen = '';
    const sink = net.createServer((sock) => {
      let inData = false;
      let buffer = '';
      sock.on('error', () => { /* the client may reset after QUIT */ });
      sock.write('220 sink\r\n');
      sock.on('data', (chunk) => {
        buffer += chunk.toString();
        let at;
        while ((at = buffer.indexOf('\r\n')) >= 0) {
          const line = buffer.slice(0, at);
          buffer = buffer.slice(at + 2);
          smtpSeen += line + '\n';
          if (inData) { if (line === '.') { inData = false; sock.write('250 queued\r\n'); } continue; }
          const up = line.toUpperCase();
          if (up.startsWith('DATA')) { inData = true; sock.write('354 go\r\n'); }
          else if (up.startsWith('QUIT')) { sock.write('221 bye\r\n'); sock.end(); }
          else sock.write('250 ok\r\n');
        }
      });
    });
    await new Promise((r) => sink.listen(0, '127.0.0.1', r));
    const E = await startServer({ QUARANTINE_DIR: qdir, REINJECT_ADDR: `127.0.0.1:${sink.address().port}` });
    section(`E. quarantine  (${E.url})`);
    const ctx = await browser.newContext({ viewport: { width: 1100, height: 800 }, locale: 'en-US' });
    const page = await ctx.newPage();
    const problems = watch(page);
    await login(page, E.url, 'root@example.com');
    await page.waitForURL(E.url + '/');
    await page.goto(E.url + '/admin');
    check('the admin sidebar links to the quarantine', await page.getByRole('link', { name: 'Quarantine' }).isVisible());
    await page.getByRole('link', { name: 'Quarantine' }).click();
    await page.waitForURL('**/admin/quarantine');
    check('the list shows both held messages', (await page.locator('tbody tr').count()) === 2);
    check('counts are shown', await page.getByText('Viruses held').isVisible());
    await page.screenshot({ path: `${SHOTS}/20-quarantine-list.png` });
    // Filters.
    await page.getByLabel('Type').selectOption('virus');
    await page.getByRole('button', { name: 'Filter' }).click();
    await page.waitForURL('**kind=virus**');
    check('filtering by type narrows the list', (await page.locator('tbody tr').count()) === 1);
    await page.goto(E.url + '/admin/quarantine');
    // Detail, and dismissing the confirmation changes nothing.
    await page.locator('tbody tr', { hasText: 'score 14.2' }).getByRole('link', { name: /^View/ }).click();
    await page.waitForURL(/\/admin\/quarantine\/[0-9a-f-]{36}$/);
    check('the detail page shows the headers', await page.getByRole('cell', { name: 'Win big' }).isVisible());
    const excerpt = await page.locator('pre.rm-excerpt').innerText();
    check('the excerpt shows markup as plain text, not as HTML', excerpt.includes('<b>hello</b>') && (await page.locator('pre.rm-excerpt b').count()) === 0, excerpt);
    await page.screenshot({ path: `${SHOTS}/21-quarantine-detail.png` });
    const itemUrl = page.url();
    page.once('dialog', (d) => d.dismiss());
    await page.getByRole('button', { name: 'Delete' }).click();
    await sleep(500);
    check('dismissing the confirmation leaves everything as it was', page.url() === itemUrl && existsSync(path.join(qdir, 'spam-e2e1')));
    // Release.
    let dialogText = '';
    page.once('dialog', (d) => { dialogText = d.message(); d.accept(); });
    await page.getByRole('button', { name: 'Release to recipient' }).click();
    await page.waitForURL('**/admin/quarantine?done=released');
    check('the confirmation warned about skipping the checks', /skips the virus and spam checks/.test(dialogText), dialogText);
    check('releasing shows a success notice', await page.getByRole('status').filter({ hasText: 'delivered to the recipient' }).isVisible());
    check('the message reached the mail server for that recipient only', /RCPT TO:<alice@example.com>/i.test(smtpSeen) && /Subject: Win big/.test(smtpSeen) && !/Delivered-To/.test(smtpSeen), smtpSeen);
    check('the released item left the held list', (await page.locator('tbody tr').count()) === 1);
    // Delete the remaining (virus) item.
    await page.locator('tbody tr').getByRole('link', { name: /^View/ }).click();
    page.once('dialog', (d) => d.accept());
    await page.getByRole('button', { name: 'Delete' }).click();
    await page.waitForURL('**/admin/quarantine?done=deleted');
    check('deleting shows a notice', await page.getByRole('status').filter({ hasText: 'was deleted' }).isVisible());
    check('…and removes the file', !existsSync(path.join(qdir, 'virus-e2e2')));
    check('no console errors on the quarantine pages', problems.length === 0, problems.join(' | '));
    await ctx.close();
    // Everyone else is kept out.
    for (const who of ['dana', 'alice']) {
      const c2 = await browser.newContext({ locale: 'en-US' });
      const p2 = await c2.newPage();
      await login(p2, E.url, `${who}@example.com`);
      await p2.waitForURL(E.url + '/');
      const r = await p2.goto(E.url + '/admin/quarantine');
      if (who === 'dana') {
        // Phase 7: domain administrators may use it, for their own domain.
        check('dana (domain admin) can open the quarantine', r.status() === 200 && (await p2.getByRole('heading', { name: 'Quarantine' }).isVisible()), String(r.status()));
      } else {
        check(`${who} gets "Access denied", not the list`, r.status() === 403 && (await p2.getByText('Access denied').first().isVisible()), String(r.status()));
      }
      await c2.close();
    }
    E.stop(); sink.close();
  }
} catch (err) {
  console.log(`\nSCRIPT ERROR: ${err.stack || err}`);
  failures.push(`script error: ${err.message}`);
} finally {
  for (const s of servers) s.kill('SIGINT');
  await browser.close();
  try { psql('postgres', `DROP DATABASE IF EXISTS ${DB} WITH (FORCE)`); } catch { /* best effort */ }
}

console.log(`\n${pass} checks passed, ${failures.length} failed`);
if (failures.length) { console.log('FAILED:\n - ' + failures.join('\n - ')); }
console.log(`screenshots: ${SHOTS}`);
console.log(failures.length ? 'E2E_RESULT=FAIL' : 'E2E_RESULT=PASS');
process.exit(failures.length ? 1 : 0);
