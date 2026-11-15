// Filters, Sieve and autoresponder end-to-end test (Phase 8): a real browser against the
// real server, database, Postfix, Dovecot (ManageSieve + Pigeonhole), Amavis and Mailpit.
//
//   make mta-up              # the dev mail stack must be running
//   cd scripts/browser-smoke && npm install && npm run filters
//
// The phase verification: a user creates the filter "from boss@ -> Boss folder" in the UI,
// a message is delivered through Postfix, and it lands in the Boss folder. It also covers the
// CodeMirror editor under the strict CSP, a script Dovecot rejects, the autoresponder (with
// and without a date range), and the fallback when the mail server cannot be reached.
// It creates a throw-away domain and user and removes them again. Needs: psql, docker,
// Chrome (CHROME_PATH), built rustmail-web and rustmail-cli binaries.
import { chromium } from 'playwright-core';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, readdirSync, existsSync, rmSync } from 'node:fs';
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
const MAILPIT = process.env.MAILPIT_API || 'http://localhost:8025/api/v1';
const SHOTS = process.env.E2E_SHOTS || path.join(os.tmpdir(), 'rustmail-filters-smoke');
mkdirSync(SHOTS, { recursive: true });
const RUN = Date.now();
const DOMAIN = `fl${RUN}.test`;
const ALICE = `alice@${DOMAIN}`;
const PW = 'correct-horse-battery-1';
const VMAIL = path.join(R, 'var/vhosts');
const HOME = path.join(VMAIL, DOMAIN, 'alice');
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
function deliver(envelopeFrom, to, raw) {
  const r = run('docker', ['exec', '-i', 'rustmail-mta', 'swaks', '--server', '127.0.0.1', '--from', envelopeFrom, '--to', to, '--data', '-', '--hide-all'], { input: raw, allowFail: true });
  return r.status === 0;
}
function message({ from, to = ALICE, subject }) {
  return `From: ${from}\r\nTo: ${to}\r\nSubject: ${subject}\r\nMessage-ID: <${crypto.randomUUID()}@example.org>\r\nDate: Tue, 11 Nov 2025 09:30:00 +0000\r\n\r\nHello.\r\n`;
}
function freePort() {
  return new Promise((res) => { const s = net.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => res(p)); }); });
}
async function startServer(extraEnv = {}) {
  const port = await freePort();
  const proc = spawn(WEB, [], {
    cwd: os.tmpdir(),
    env: {
      PATH: process.env.PATH, DATABASE_URL: DB_URL, JWT_SECRET: crypto.randomBytes(32).toString('hex'),
      WEB_BIND_ADDR: `127.0.0.1:${port}`, COOKIE_SECURE: 'false', RUST_LOG: 'warn', ASSET_SOURCE: 'local',
      LOGIN_RATE_BURST: '100', LOGIN_RATE_PERIOD_SECS: '1', VMAIL_BASE: VMAIL,
      IMAP_HOST: '127.0.0.1', IMAP_PORT: '1993', SMTP_HOST: '127.0.0.1', SMTP_PORT: '5587',
      MAIL_TLS_INSECURE: '1', MAIL_MASTER_USER: 'webmail', MAIL_MASTER_PASSWORD: MASTER_PASSWORD,
      ...extraEnv,
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
    // Pages that are expected to refuse something make the browser log the 4xx itself.
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
async function eventually(fn, seconds = 40) {
  for (let i = 0; i < seconds * 2; i++) {
    try { const v = await fn(); if (v) return v; } catch { /* retry */ }
    await sleep(500);
  }
  return false;
}
async function submit(page, urlPart, button) {
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.request().method() === 'POST' && r.url().includes(urlPart), { timeout: 20000 }),
    button.click(),
  ]);
  return response;
}
async function mailpitCount(query) {
  try { return (await (await fetch(`${MAILPIT}/search?query=${encodeURIComponent(query)}`)).json()).messages_count || 0; } catch { return 0; }
}
/** Subjects of the messages in a folder as the webmail list shows them. */
async function folderSubjects(page, base, folder) {
  await page.goto(`${base}/mail?folder=${encodeURIComponent(folder)}`);
  return (await page.locator('#mail-list .rm-row').allInnerTexts()).join(' | ');
}
async function fillRule(page, { name, from, folder, markRead = false }) {
  await page.goto(page.url().split('/settings')[0] + '/settings/filters/rule');
  await page.locator('#r-name').fill(name);
  await page.locator('#c1-field').selectOption('from');
  await page.locator('#c1-op').selectOption('contains');
  await page.locator('#c1-value').fill(from);
  await page.locator('#r-new-folder').fill(folder);
  if (markRead) await page.locator('#r-read').check();
  return submit(page, '/settings/filters/rule', page.getByRole('button', { name: 'Save' }));
}

// ── setup ───────────────────────────────────────────────────────────────────
section('Setup');
cli(['domain', 'create', DOMAIN]);
cli(['user', 'create', ALICE, '--password', PW, '--name', 'Alice Anders']);
check('a throw-away domain and user exist', existsSync(HOME));

const server = await startServer();
const offline = await startServer({ SIEVE_PORT: '1' }); // nothing listens on port 1
const browser = await chromium.launch({ executablePath: CHROME, headless: true });
try {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US' });
  const page = await ctx.newPage();
  const problems = watch(page);
  await login(page, server.url, ALICE);

  // ── the verification: from boss@ -> Boss folder ────────────────────────────
  section('Rule: from boss@ goes to the Boss folder');
  await page.goto(server.url + '/settings/filters');
  check('a new user has no rules', await page.getByTestId('filters-empty').isVisible());
  const saved = await fillRule(page, { name: 'Boss', from: 'boss@', folder: 'Boss', markRead: true });
  check('saving the rule succeeds', saved.ok(), String(saved.status()));
  await page.waitForURL('**/settings/filters?done=saved');
  check('the page says the filters are active on the mail server', (await page.getByTestId('filters-status').innerText()).includes('Active on the mail server'));
  check('the rule is listed in words', (await page.getByTestId('filters-table').innerText()).includes('Sender contains “boss@”') && (await page.getByTestId('filters-table').innerText()).includes('move to Boss'));
  await page.screenshot({ path: `${SHOTS}/01-rule.png` });
  const filtersFile = path.join(HOME, 'sieve', 'rustmail-filters.sieve');
  check('Dovecot stored the generated script', existsSync(filtersFile) && /fileinto :create "Boss"/.test(readFileSync(filtersFile, 'utf8')));
  check('the main script is the active one', existsSync(path.join(HOME, '.dovecot.sieve')) && /include :personal :optional "rustmail-filters"/.test(readFileSync(path.join(HOME, '.dovecot.sieve'), 'utf8')));

  check('a message from boss@ is delivered', deliver('boss@bigcorp.example', ALICE, message({ from: 'The Boss <boss@bigcorp.example>', subject: `Quarterly plan ${RUN}` })));
  check('a message from someone else is delivered', deliver('friend@example.org', ALICE, message({ from: 'Friend <friend@example.org>', subject: `Lunch ${RUN}` })));
  const routed = await eventually(async () => (await folderSubjects(page, server.url, 'Boss')).includes(`Quarterly plan ${RUN}`), 90);
  check('the boss message arrived in the Boss folder', routed);
  const inbox = await eventually(async () => (await folderSubjects(page, server.url, 'INBOX')).includes(`Lunch ${RUN}`), 60);
  check('the other message stayed in the inbox', inbox);
  check('…and the boss message is not in the inbox', !(await folderSubjects(page, server.url, 'INBOX')).includes('Quarterly plan'));
  await page.goto(`${server.url}/mail?folder=Boss`);
  check('the rule marked it as read', (await page.locator('#mail-list .rm-row.unread').count()) === 0 && (await page.locator('#mail-list .rm-row').count()) === 1);
  await page.screenshot({ path: `${SHOTS}/02-boss-folder.png` });

  // ── the advanced editor ────────────────────────────────────────────────────
  section('Advanced editor (CodeMirror)');
  await page.goto(server.url + '/settings/filters/advanced');
  const ready = await eventually(async () => (await page.locator('#script-form').getAttribute('data-editor-ready')) === 'true', 15);
  check('the CodeMirror editor loads from the local bundle', ready);
  check('…and replaces the textarea', (await page.locator('.cm-editor').count()) === 1 && !(await page.locator('#script').isVisible()));
  check('it starts from the script the rules generate', (await page.locator('.cm-content').innerText()).includes('fileinto :create "Boss"'));
  await page.screenshot({ path: `${SHOTS}/03-editor.png` });
  const typeScript = async (text) => {
    await page.locator('.cm-content').click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.press('Delete');
    // Real key events, as a person types. (CDP's insertText makes Chrome's own editing engine
    // add style spans, which the strict CSP rightly blocks; CodeMirror itself does not.)
    await page.keyboard.type(text);
  };
  await typeScript('fileintoo "Nowhere";');
  let r = await submit(page, '/settings/filters/advanced', page.getByRole('button', { name: 'Check and save' }));
  const refusal = await page.locator('#script-status .alert-danger').innerText();
  check('a script Dovecot cannot compile is refused with its explanation', r.status() === 422 && /fileintoo/.test(refusal), `${r.status()} ${refusal}`);
  await page.goto(server.url + '/settings/filters');
  check('…and nothing was changed', (await page.getByTestId('filters-table').count()) === 1);

  await page.goto(server.url + '/settings/filters/advanced');
  await page.locator('.cm-content').waitFor();
  await typeScript('require ["fileinto", "mailbox"];\nif header :contains "subject" "invoice" { fileinto :create "Invoices"; }\n');
  r = await submit(page, '/settings/filters/advanced', page.getByRole('button', { name: 'Check and save' }));
  check('a valid script is accepted', r.ok(), String(r.status()));
  await page.waitForURL('**/settings/filters/advanced?done=saved');
  check('the script is live on the server', /Invoices/.test(readFileSync(filtersFile, 'utf8')));
  check('the rule list explains that the script is in use', (await (async () => { await page.goto(server.url + '/settings/filters'); return page.locator('main').innerText(); })()).includes('your own Sieve script'));
  check('an invoice goes to Invoices', deliver('billing@example.org', ALICE, message({ from: 'Billing <billing@example.org>', subject: `Invoice 42 ${RUN}` })));
  check('…and arrives there', await eventually(async () => (await folderSubjects(page, server.url, 'Invoices')).includes(`Invoice 42 ${RUN}`), 90));
  await page.goto(server.url + '/settings/filters/advanced');
  page.once('dialog', (d) => d.accept());
  await submit(page, '/advanced/leave', page.getByRole('button', { name: 'Go back to the rule builder' }));
  await page.waitForURL('**/settings/filters?done=raw-left');
  check('going back restores the rule builder with the rules intact', (await page.getByTestId('filters-table').innerText()).includes('Boss'));
  check('…and the Boss script is live again', /fileinto :create "Boss"/.test(readFileSync(filtersFile, 'utf8')));

  // ── autoresponder ──────────────────────────────────────────────────────────
  section('Autoresponder');
  const fillAuto = async ({ enabled = true, starts = '', ends = '' }) => {
    await page.goto(server.url + '/settings/autoresponder');
    if (enabled) await page.locator('#a-enabled').check(); else await page.locator('#a-enabled').uncheck();
    await page.locator('#a-start').fill(starts);
    await page.locator('#a-end').fill(ends);
    await page.locator('#a-subject').fill(`Out of office ${RUN}`);
    await page.locator('#a-body').fill('I am away and will answer when I am back.');
    await page.locator('#a-days').fill('1');
    return submit(page, '/settings/autoresponder', page.getByRole('button', { name: 'Save' }));
  };
  // A date range that has not started yet: no reply.
  r = await fillAuto({ starts: '2099-01-01', ends: '2099-01-31' });
  await page.waitForURL('**/settings/autoresponder?done=saved');
  check('a future date range is saved and live', (await page.getByTestId('auto-status').innerText()).includes('Active on the mail server'));
  check('the vacation script has the date test', /currentdate :value "ge" "date" "2099-01-01"/.test(readFileSync(path.join(HOME, 'sieve', 'rustmail-vacation.sieve'), 'utf8')));
  check('mail arrives before the range starts', deliver('early@example.org', ALICE, message({ from: 'Early <early@example.org>', subject: `Early ${RUN}` })));
  // Dovecot judges the script when it delivers, so wait until it has before changing it.
  check('…and is delivered', await eventually(async () => (await folderSubjects(page, server.url, 'INBOX')).includes(`Early ${RUN}`), 90));
  // No dates: replies are sent.
  r = await fillAuto({});
  await page.waitForURL('**/settings/autoresponder?done=saved');
  check('an open-ended autoresponder is saved and live', (await page.getByTestId('auto-status').innerText()).includes('Active on the mail server'));
  check('mail arrives once it is on', deliver('customer@example.org', ALICE, message({ from: 'Customer <customer@example.org>', subject: `Question ${RUN}` })));
  const replied = await eventually(async () => (await mailpitCount(`subject:"Out of office ${RUN}" to:customer@example.org`)) > 0, 90);
  check('the customer gets the automatic reply', replied);
  check('the sender of the earlier mail got none', (await mailpitCount(`subject:"Out of office ${RUN}" to:early@example.org`)) === 0);
  check('the autoresponder did not stop filtering', /fileinto :create "Boss"/.test(readFileSync(filtersFile, 'utf8')));
  await page.screenshot({ path: `${SHOTS}/04-autoresponder.png` });
  r = await fillAuto({ enabled: false });
  await page.waitForURL('**/settings/autoresponder?done=saved');
  check('switching it off installs an empty script', /autoresponder is off/.test(readFileSync(path.join(HOME, 'sieve', 'rustmail-vacation.sieve'), 'utf8')));

  // ── the mail server cannot be reached ──────────────────────────────────────
  section('When the mail server cannot be reached');
  check('a press release is delivered before the rule exists', deliver('press@example.org', ALICE, message({ from: 'Press <press@example.org>', subject: `Press release ${RUN}` })));
  check('it reaches the inbox', await eventually(async () => (await folderSubjects(page, server.url, 'INBOX')).includes(`Press release ${RUN}`), 90));
  const c2 = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US' });
  const p2 = await c2.newPage();
  const problems2 = watch(p2);
  await login(p2, offline.url, ALICE);
  await p2.goto(offline.url + '/settings/filters/rule');
  await p2.locator('#r-name').fill('Press');
  await p2.locator('#c1-value').fill('press@');
  await p2.locator('#r-new-folder').fill('Press');
  const pending = await submit(p2, '/settings/filters/rule', p2.getByRole('button', { name: 'Save' }));
  check('the rule is saved', pending.ok());
  await p2.waitForURL('**/settings/filters?done=pending');
  check('the page says it is not active yet and why', (await p2.getByTestId('filters-status').innerText()).includes('Not active yet') && /cannot reach the mail server/i.test(await p2.locator('main').innerText()));
  check('a "Send again" button is offered', (await p2.getByRole('button', { name: 'Send again' }).count()) === 1);
  const retry = await submit(p2, '/settings/filters/retry', p2.getByRole('button', { name: 'Send again' }));
  check('sending again while it is down reports the problem', retry.status() === 422 && (await p2.locator('#filters-action-status .alert-danger').isVisible()));
  p2.once('dialog', (d) => d.accept());
  await submit(p2, '/settings/filters/apply', p2.getByRole('button', { name: 'Apply to inbox' }));
  await p2.waitForURL('**/settings/filters?done=applied**');
  check('"Apply to inbox" reports what it did', /applied to your inbox: \d+ changes/.test(await p2.getByRole('status').first().innerText()));
  check('the press release was moved by the fallback engine', (await folderSubjects(p2, offline.url, 'Press')).includes(`Press release ${RUN}`));
  check('…and left the inbox', !(await folderSubjects(p2, offline.url, 'INBOX')).includes(`Press release ${RUN}`));
  check('the offline pages had no console errors', problems2.length === 0, problems2.join(' | '));
  await c2.close();

  check('no console or CSP errors on the filter pages (editor and charts included)', problems.length === 0, problems.join(' | '));
  await ctx.close();
} catch (err) {
  console.log(`\nSCRIPT ERROR: ${err.stack || err}`);
  failures.push(`script error: ${err.message}`);
} finally {
  await browser.close();
  server.stop();
  offline.stop();
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
