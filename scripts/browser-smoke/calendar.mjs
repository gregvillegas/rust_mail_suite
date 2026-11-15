// Calendar and contacts end-to-end test (Phase 9): a real browser, the real server and
// database, and an independent CalDAV client (the Python `caldav` library).
//
//   cd scripts/dav-smoke && python3 -m venv .venv && .venv/bin/pip install -r requirements.txt
//   cd scripts/browser-smoke && npm install && npm run calendar
//
// Set DAV_PYTHON to a Python that has `caldav` installed if it is not in scripts/dav-smoke/.venv.
// Thunderbird itself cannot be driven from a script, so the Python client stands in for it: it
// discovers calendars from the server address, creates, searches, changes, syncs and shares
// the way any CalDAV app does.
//
// It creates a throw-away domain with two users and removes it again. Needs: psql, Chrome
// (CHROME_PATH), Python with `caldav`, built rustmail-web and rustmail-cli binaries.
process.env.TZ = 'Europe/Berlin'; // the browser's zone, so converting times is part of the test
import { chromium } from 'playwright-core';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, existsSync, rmSync } from 'node:fs';
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
const SHOTS = process.env.E2E_SHOTS || path.join(os.tmpdir(), 'rustmail-calendar-smoke');
const VENV_PYTHON = `${R}/scripts/dav-smoke/.venv/bin/python`;
const PYTHON = process.env.DAV_PYTHON || (existsSync(VENV_PYTHON) ? VENV_PYTHON : 'python3');
const CLIENT = `${R}/scripts/dav-smoke/client.py`;
mkdirSync(SHOTS, { recursive: true });
const RUN = Date.now();
const DOMAIN = `cal${RUN}.test`;
const ALICE = `alice@${DOMAIN}`;
const BOB = `bob@${DOMAIN}`;
const PW = 'correct-horse-battery-1';
const VMAIL = path.join(R, 'var/vhosts');

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
function freePort() {
  return new Promise((res) => { const s = net.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); });
}
async function startServer() {
  const port = await freePort();
  const proc = spawn(WEB, [], {
    cwd: os.tmpdir(),
    env: { PATH: process.env.PATH, TZ: 'UTC', DATABASE_URL: DB_URL, JWT_SECRET: crypto.randomBytes(32).toString('hex'), WEB_BIND_ADDR: `127.0.0.1:${port}`, COOKIE_SECURE: 'false', RUST_LOG: 'warn', ASSET_SOURCE: 'local', LOGIN_RATE_BURST: '100', LOGIN_RATE_PERIOD_SECS: '1' },
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
    if (/Failed to load resource: the server responded with a status of (40[34]|422)/.test(m.text())) return;
    problems.push(m.text());
  });
  page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`));
  return problems;
}
async function login(page, base, email) {
  await page.goto(base + '/login');
  await page.locator('input[name="email"]').fill(email);
  await page.locator('#login-password').fill(PW);
  await page.locator('#login-form button[type="submit"]').click();
  await page.waitForURL(base + '/', { timeout: 15000 });
}
async function submit(page, urlPart, button) {
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.request().method() === 'POST' && r.url().includes(urlPart), { timeout: 20000 }),
    button.click(),
  ]);
  return response;
}
/** Runs a scenario of the Python CalDAV client and reports each of its checks. */
function pythonClient(server, user, token, scenario, env = {}) {
  const r = spawnSync(PYTHON, [CLIENT, scenario], { encoding: 'utf8', env: { ...process.env, DAV_URL: `${server.url}/dav/`, DAV_USER: user, DAV_TOKEN: token, ...env } });
  let any = false;
  for (const line of r.stdout.split('\n')) {
    const m = /^(PASS|FAIL) (.*)$/.exec(line);
    if (!m) continue;
    any = true;
    const [label, detail] = m[2].split(' -> ');
    check(`[${scenario}] ${label}`, m[1] === 'PASS', detail);
  }
  if (!any || r.status === null) check(`[${scenario}] the client ran`, false, (r.stderr || r.stdout).slice(-400));
  return r;
}
async function createAppPassword(page, base, name) {
  await page.goto(base + '/settings/apps');
  await page.locator('#app-name').fill(name);
  await submit(page, '/settings/apps', page.getByRole('button', { name: 'Create' }));
  const token = (await page.getByTestId('app-token').innerText()).trim();
  return token;
}
/** Local `YYYY-MM-DD` for today plus `days`, in the zone of this process (Europe/Berlin). */
function ymd(days) {
  const d = new Date(Date.now() + days * 86400000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}
const basic = (user, token) => `Basic ${Buffer.from(`${user}:${token}`).toString('base64')}`;
async function dav(server, method, urlPath, user, token, body, headers = {}) {
  return fetch(server.url + urlPath, { method, body, headers: { authorization: basic(user, token), ...headers } });
}

// ── setup ───────────────────────────────────────────────────────────────────
section('Setup');
cli(['domain', 'create', DOMAIN]);
cli(['user', 'create', ALICE, '--password', PW, '--name', 'Alice']);
cli(['user', 'create', BOB, '--password', PW, '--name', 'Bob']);
check('a throw-away domain with two users exists', true);
const pyCheck = spawnSync(PYTHON, ['-c', 'import caldav'], { encoding: 'utf8' });
check('the Python caldav client is available', pyCheck.status === 0, pyCheck.stderr);

const server = await startServer();
const browser = await chromium.launch({ executablePath: CHROME, headless: true });
try {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 1000 }, locale: 'en-US', timezoneId: 'Europe/Berlin' });
  const page = await ctx.newPage();
  const problems = watch(page);
  await login(page, server.url, ALICE);

  // ── app password ───────────────────────────────────────────────────────────
  section('App password');
  const token = await createAppPassword(page, server.url, 'Thunderbird laptop');
  check('an app password is shown once', token.length === 32);
  check('…with the addresses to set the app up', (await page.locator('#app-result').innerText()).includes(`/dav/calendars/${ALICE}/default/`));
  await page.goto(server.url + '/settings/apps');
  check('the list shows it by name, never the secret', (await page.getByTestId('apps-table').innerText()).includes('Thunderbird laptop') && !(await page.content()).includes(token));
  const wellKnown = await fetch(server.url + '/.well-known/caldav', { redirect: 'manual' });
  check('/.well-known/caldav points apps at the server', wellKnown.status === 301 && wellKnown.headers.get('location') === '/dav/');
  const noAuth = await fetch(server.url + '/dav/', { method: 'PROPFIND' });
  check('the DAV server asks for a password', noAuth.status === 401 && /Basic/.test(noAuth.headers.get('www-authenticate') || ''));
  const withLogin = await dav(server, 'PROPFIND', '/dav/', ALICE, PW, '', { depth: '0' });
  check('the account password is not accepted there', withLogin.status === 401);

  // ── the calendar page ──────────────────────────────────────────────────────
  section('Calendar view');
  await page.goto(server.url + '/calendar');
  await page.locator('#calendar-root[data-calendar-ready="true"]').waitFor({ timeout: 20000 });
  check('FullCalendar loads from the local bundle and draws a month', (await page.locator('.fc-daygrid').count()) === 1);
  check('the toolbar is in the page language', (await page.locator('.fc-today-button').innerText()).toLowerCase().includes('today'));
  await page.locator('.fc-timeGridWeek-button').click();
  check('the week view can be opened', await page.locator('.fc-timegrid').isVisible());
  await page.locator('.fc-dayGridMonth-button').click();
  check('the default calendar is listed', (await page.getByTestId('calendar-list').innerText()).includes('Calendar'));
  await page.screenshot({ path: `${SHOTS}/01-empty-calendar.png` });

  // ── an event made in the page, seen by an app ──────────────────────────────
  section('An event made in the page');
  const day = ymd(3);
  await page.getByRole('link', { name: 'New event' }).first().click();
  await page.waitForURL('**/calendar/event');
  await page.locator('#event-form[data-form-ready="true"]').waitFor();
  await page.locator('#e-title').fill('UI event: planning');
  await page.locator('#e-location').fill('Room 4');
  await page.locator('#e-start').fill(`${day}T09:00`);
  await page.locator('#e-end').fill(`${day}T10:30`);
  let r = await submit(page, '/calendar/event', page.getByRole('button', { name: 'Save' }));
  check('saving the event succeeds', r.ok(), String(r.status()));
  await page.waitForURL('**/calendar?date=*');
  await page.locator('#calendar-root[data-calendar-ready="true"]').waitFor();
  const shown = page.locator('.fc-event', { hasText: 'UI event: planning' });
  await shown.first().waitFor({ timeout: 15000 });
  check('it appears in the calendar view', (await shown.count()) >= 1);
  const expectedUtc = new Date(`${day}T09:00:00`).toISOString().replace(/[-:]/g, '').replace(/\.\d+Z$/, 'Z');
  // A repeating event, written in the page's time zone so it keeps its wall-clock time.
  await page.goto(server.url + '/calendar/event');
  await page.locator('#event-form[data-form-ready="true"]').waitFor();
  await page.locator('#e-title').fill('UI weekly standup');
  await page.locator('#e-start').fill(`${day}T08:00`);
  await page.locator('#e-end').fill(`${day}T08:15`);
  await page.locator('#e-repeat').selectOption('weekly');
  await submit(page, '/calendar/event', page.getByRole('button', { name: 'Save' }));
  await page.waitForURL('**/calendar?date=*');
  await page.locator('.fc-event', { hasText: 'UI weekly standup' }).first().waitFor({ timeout: 15000 });
  check('a repeating event shows more than once in a month', (await page.locator('.fc-event', { hasText: 'UI weekly standup' }).count()) >= 1);
  await page.screenshot({ path: `${SHOTS}/02-events.png` });
  pythonClient(server, ALICE, token, 'ui-event', { EXPECT_START_UTC: expectedUtc });

  // ── an app's events, seen in the page ──────────────────────────────────────
  section('Events made by a calendar app');
  const general = pythonClient(server, ALICE, token, 'alice');
  check('the general client run finished cleanly', general.status === 0, general.stdout.split('\n').filter((l) => l.startsWith('FAIL')).join(' | '));
  pythonClient(server, ALICE, token, 'app-event');
  await page.goto(server.url + `/calendar?date=${ymd(2)}`);
  await page.locator('#calendar-root[data-calendar-ready="true"]').waitFor();
  await page.locator('.fc-event', { hasText: 'Made in the app' }).first().waitFor({ timeout: 15000 });
  check('an event made by the app appears in the page', true);
  await page.locator('.fc-event', { hasText: 'Board meeting' }).first().click();
  await page.waitForURL('**/calendar/event?*');
  check('an event with a reminder and a guest is shown but not editable here', await page.getByTestId('event-readonly').isVisible() && (await page.locator('#event-form').count()) === 0);
  page.once('dialog', (d) => d.accept());
  await submit(page, '/calendar/event/delete', page.getByRole('button', { name: 'Delete' }));
  await page.waitForURL('**/calendar?done=deleted');
  check('…but it can be deleted', true);

  // Editing an event made in the page, by clicking it.
  await page.goto(server.url + `/calendar?date=${day}`);
  await page.locator('#calendar-root[data-calendar-ready="true"]').waitFor();
  await page.locator('.fc-event', { hasText: 'UI event: planning' }).first().click();
  await page.waitForURL('**/calendar/event?*');
  await page.locator('#event-form[data-form-ready="true"]').waitFor();
  check('clicking an event opens it with its times in the local zone', (await page.locator('#e-start').inputValue()) === `${day}T09:00`, await page.locator('#e-start').inputValue());
  await page.locator('#e-title').fill('UI event: planning (renamed)');
  await submit(page, '/calendar/event', page.getByRole('button', { name: 'Save' }));
  await page.waitForURL('**/calendar?date=*');
  await page.locator('.fc-event', { hasText: 'renamed' }).first().waitFor();
  check('the changed event shows its new title', true);

  // ── contacts ───────────────────────────────────────────────────────────────
  section('Contacts');
  await page.goto(server.url + '/contacts');
  await page.getByRole('link', { name: 'New contact' }).first().click();
  await page.locator('#k-name').fill('Ada Lovelace');
  await page.locator('#k-org').fill('Analytical Engines');
  await page.locator('#k-emails').fill('ada@example.org\nada@home.example');
  await page.locator('#k-phones').fill('+44 20 7946 0000');
  r = await submit(page, '/contacts/save', page.getByRole('button', { name: 'Save' }));
  check('saving a contact succeeds', r.ok(), String(r.status()));
  await page.waitForURL('**/contacts?*');
  check('it is in the list', (await page.getByTestId('contacts-table').innerText()).includes('ada@home.example'));
  const book = `/dav/addressbooks/${ALICE}/contacts/`;
  const query = '<a:addressbook-query xmlns:d="DAV:" xmlns:a="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/><a:address-data/></d:prop><a:filter><a:prop-filter name="EMAIL"><a:text-match match-type="contains">ada@home</a:text-match></a:prop-filter></a:filter></a:addressbook-query>';
  let report = await (await dav(server, 'REPORT', book, ALICE, token, query, { depth: '1', 'content-type': 'application/xml' })).text();
  check('a contacts app finds it with an address-book query', report.includes('FN:Ada Lovelace') && report.includes('ada@example.org'), report.slice(0, 200));
  // A phone adds a contact with a photo and a custom field.
  const phoneCard = 'BEGIN:VCARD\r\nVERSION:3.0\r\nUID:phone-1\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nEMAIL:grace@navy.example\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ\r\nX-CUSTOM:keep me\r\nEND:VCARD\r\n';
  const put = await dav(server, 'PUT', `${book}grace.vcf`, ALICE, token, phoneCard, { 'content-type': 'text/vcard', 'if-none-match': '*' });
  check('a contacts app can add a contact', put.status === 201, String(put.status));
  await page.goto(server.url + '/contacts?q=grace');
  await page.getByRole('link', { name: 'Grace Hopper' }).click();
  await page.locator('#k-notes').fill('Edited in the web page');
  await submit(page, '/contacts/save', page.getByRole('button', { name: 'Save' }));
  const stored = await (await dav(server, 'GET', `${book}grace.vcf`, ALICE, token)).text();
  check('editing in the page keeps the photo and custom field the page does not show', stored.includes('PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ') && stored.includes('X-CUSTOM:keep me') && stored.includes('Edited in the web page'), stored);
  await page.goto(server.url + '/contacts?q=%25');
  check('searching for % does not match everything', await page.getByTestId('contacts-empty').isVisible());
  await page.screenshot({ path: `${SHOTS}/03-contacts.png` });

  // ── sharing with a colleague ───────────────────────────────────────────────
  section('Sharing');
  await page.goto(server.url + '/settings/collections');
  const card = page.getByTestId('collection-card').first();
  check('the calendar shows the address an app needs', (await card.innerText()).includes(`/dav/calendars/${ALICE}/default/`));
  await card.locator('input[name="with_user"]').fill('someone@elsewhere.example');
  r = await submit(page, '/share', card.getByRole('button', { name: 'Share', exact: true }));
  check('sharing with someone who has no account here is refused with a reason', r.status() === 422 && (await card.locator('[id^=share-] .alert-danger').isVisible()));
  await card.locator('input[name="with_user"]').fill(BOB);
  r = await submit(page, '/share', card.getByRole('button', { name: 'Share', exact: true }));
  await page.waitForURL('**/settings/collections?done=shared');
  check('it can be shared with a colleague, read-only', (await page.getByTestId('collection-card').first().innerText()).includes(BOB));
  await page.screenshot({ path: `${SHOTS}/04-sharing.png`, fullPage: true });

  const bobCtx = await browser.newContext({ viewport: { width: 1280, height: 1000 }, locale: 'en-US', timezoneId: 'Europe/Berlin' });
  const bobPage = await bobCtx.newPage();
  const bobProblems = watch(bobPage);
  await login(bobPage, server.url, BOB);
  const bobToken = await createAppPassword(bobPage, server.url, 'Phone');
  await bobPage.goto(server.url + `/calendar?date=${day}`);
  await bobPage.locator('#calendar-root[data-calendar-ready="true"]').waitFor();
  await bobPage.locator('.fc-event', { hasText: 'UI event: planning' }).first().waitFor({ timeout: 15000 });
  check("the colleague sees alice's event in their own calendar", true);
  check("…and the calendar is listed as hers, read-only", (await bobPage.getByTestId('calendar-list').innerText()).includes('Read only'));
  await bobPage.locator('.fc-event', { hasText: 'UI event: planning' }).first().click();
  await bobPage.waitForURL('**/calendar/event?*');
  check('they cannot edit it', await bobPage.getByTestId('event-readonly').isVisible() && (await bobPage.getByRole('button', { name: 'Delete' }).count()) === 0);
  pythonClient(server, BOB, bobToken, 'shared');
  await bobPage.goto(server.url + `/calendar/freebusy?user=${encodeURIComponent(ALICE)}&date=${day}`);
  const table = await bobPage.getByTestId('freebusy-table').innerText();
  check("free/busy shows when alice is busy, without titles", /\d\d:\d\d – \d\d:\d\d/.test(table) && !table.includes('planning'), table.slice(0, 300));
  await bobPage.goto(server.url + `/calendar/freebusy?user=${encodeURIComponent('nobody@' + DOMAIN)}`);
  check('free/busy for someone who does not exist says so', await bobPage.getByText('no one with that address').isVisible());
  check("bob's pages had no console or CSP errors", bobProblems.length === 0, bobProblems.join(' | '));
  await bobCtx.close();

  // ── German ─────────────────────────────────────────────────────────────────
  section('German');
  const deCtx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'de-DE', timezoneId: 'Europe/Berlin' });
  const dePage = await deCtx.newPage();
  const deProblems = watch(dePage);
  await login(dePage, server.url, ALICE);
  await dePage.goto(server.url + '/calendar');
  await dePage.locator('#calendar-root[data-calendar-ready="true"]').waitFor();
  check('the calendar toolbar is in German', (await dePage.locator('.fc-today-button').innerText()) === 'Heute', await dePage.locator('.fc-today-button').innerText());
  check('the page text is German', (await dePage.locator('h1').first().innerText()) === 'Kalender');
  await dePage.goto(server.url + '/contacts');
  check('contacts are German too', (await dePage.locator('h1').first().innerText()) === 'Kontakte');
  check('German pages had no console or CSP errors', deProblems.length === 0, deProblems.join(' | '));
  await deCtx.close();

  // ── revoking ───────────────────────────────────────────────────────────────
  section('Revoking');
  await page.goto(server.url + '/settings/apps');
  page.once('dialog', (d) => d.accept());
  await submit(page, '/revoke', page.getByRole('button', { name: /Revoke/ }));
  await page.waitForURL('**/settings/apps?done=revoked');
  const after = await dav(server, 'PROPFIND', '/dav/', ALICE, token, '', { depth: '0' });
  check('a revoked app password stops working at once', after.status === 401);

  check('no console or CSP errors on the calendar, contacts and settings pages', problems.length === 0, problems.join(' | '));
  await ctx.close();
} catch (err) {
  console.log(`\nSCRIPT ERROR: ${err.stack || err}`);
  failures.push(`script error: ${err.message}`);
} finally {
  await browser.close();
  server.stop();
  psql(`DELETE FROM users WHERE email LIKE '%@${DOMAIN}'`);
  psql(`DELETE FROM domains WHERE name = '${DOMAIN}'`);
  rmSync(path.join(VMAIL, DOMAIN), { recursive: true, force: true });
}
console.log(`\n${pass} passed, ${failures.length} failed`);
if (failures.length) {
  for (const f of failures) console.log(`  FAILED: ${f}`);
  console.log(`\nServer log:\n${server.log().slice(-3000)}`);
  process.exit(1);
}
