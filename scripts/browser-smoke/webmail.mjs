// Webmail end-to-end test (Phase 6): a real browser against the real server,
// database, Postfix, Dovecot, Amavis and Mailpit.
//
//   make mta-up              # the dev mail stack must be running
//   cd scripts/browser-smoke && npm install && npm run webmail
//
// It creates a throw-away domain with two users (alice, bob) in the dev database and
// removes it again. Needs: psql, docker, Chrome (CHROME_PATH), built rustmail-web and
// rustmail-cli binaries.
import { chromium } from 'playwright-core';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
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
const SHOTS = process.env.E2E_SHOTS || path.join(os.tmpdir(), 'rustmail-webmail-smoke');
mkdirSync(SHOTS, { recursive: true });

const RUN = Date.now();
const DOMAIN = `wm${RUN}.test`;
const ALICE = `alice@${DOMAIN}`;
const BOB = `bob@${DOMAIN}`;
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
      LOGIN_RATE_BURST: '100', LOGIN_RATE_PERIOD_SECS: '1',
      IMAP_HOST: '127.0.0.1', IMAP_PORT: '1993', SMTP_HOST: '127.0.0.1', SMTP_PORT: '5587',
      MAIL_TLS_INSECURE: '1', MAIL_MASTER_USER: 'webmail', MAIL_MASTER_PASSWORD: MASTER_PASSWORD,
      ...(process.env.WEB_ENV_EXTRA ? JSON.parse(process.env.WEB_ENV_EXTRA) : {}),
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
    const t = m.text();
    if (/Failed to load resource/.test(t)) return; // expected for blocked remote images
    problems.push(t);
  });
  page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`));
  return problems;
}
async function login(page, base, email) {
  await page.goto(base + '/login');
  await page.locator('input[name="email"]').fill(email);
  await page.locator('#login-password').fill(PW);
  await page.locator('#login-form button[type="submit"]').click();
  await page.waitForURL(base + '/');
}
/** Runs `click` and waits for the POST to `urlPart` it triggers to finish. */
async function act(page, urlPart, click) {
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.request().method() === 'POST' && r.url().includes(urlPart), { timeout: 20000 }),
    click(),
  ]);
  return response;
}
/** Polls until `fn` returns truthy or the time is up. */
async function eventually(fn, seconds = 40, label = '') {
  for (let i = 0; i < seconds * 2; i++) {
    try { const v = await fn(); if (v) return v; } catch { /* retry */ }
    await sleep(500);
  }
  return false;
}
async function mailpitHas(subject) {
  try {
    const r = await fetch(`${MAILPIT}/search?query=${encodeURIComponent(`subject:"${subject}"`)}`);
    const j = await r.json();
    return (j.messages_count || 0) > 0;
  } catch { return false; }
}

const PLAIN = `From: Olivia Outsider <outsider@example.org>\r\nTo: ${ALICE}\r\nSubject: Lunch on Thursday\r\nMessage-ID: <plain-${RUN}@example.org>\r\nDate: Tue, 11 Nov 2025 09:30:00 +0000\r\n\r\nHi Alice,\r\nare you free for pizza on Thursday?\r\nOlivia\r\n`;
const HTML = [
  'From: Newsletter <news@example.org>', `To: ${ALICE}`, 'Subject: Weekly report', `Message-ID: <html-${RUN}@example.org>`,
  'Date: Tue, 11 Nov 2025 10:00:00 +0000', 'MIME-Version: 1.0', 'Content-Type: multipart/mixed; boundary="BND"', '',
  '--BND', 'Content-Type: text/html; charset=utf-8', '',
  '<h2 id="hello">Quarterly numbers</h2><p>See <a href="https://example.com/report">the report</a>.</p>' +
  '<script>document.title="pwned";window.__pwned=1</script><img src="https://tracker.example/pixel.gif?u=alice" width="1" height="1" alt="">' +
  '<img src="x" onerror="window.__pwned=2"><a href="javascript:window.__pwned=3">click</a><iframe src="https://evil.example"></iframe>',
  '--BND', 'Content-Type: text/plain; name="notes.txt"', 'Content-Disposition: attachment; filename="notes.txt"', 'Content-Transfer-Encoding: base64', '',
  Buffer.from('attachment content: lasagna recipe').toString('base64'), '--BND--', '',
].join('\r\n');

// ── setup ───────────────────────────────────────────────────────────────────
section('Setup');
cli(['domain', 'create', DOMAIN]);
cli(['user', 'create', ALICE, '--password', PW, '--name', 'Alice Anders']);
cli(['user', 'create', BOB, '--password', PW, '--name', 'Bob Baker']);
check('users created', true);
check('plain message delivered to Postfix', deliver(ALICE, PLAIN));
check('HTML message with an attachment delivered to Postfix', deliver(ALICE, HTML));

const server = await startServer();
const browser = await chromium.launch({ executablePath: CHROME, headless: true });
const tmp = mkdtempSync(path.join(os.tmpdir(), 'wm-upload-'));
try {
  // ── anonymous and wrong-origin access ─────────────────────────────────────
  section('Access control');
  {
    const r = await fetch(server.url + '/mail', { redirect: 'manual' });
    check('/mail redirects anonymous visitors to sign-in', r.status === 303 && (r.headers.get('location') || '').startsWith('/login'), String(r.status));
    for (const p of ['/mail/message/1?folder=INBOX', '/mail/compose', '/mail/folders', '/mail/list']) {
      const x = await fetch(server.url + p, { redirect: 'manual' });
      check(`${p} needs a session`, x.status === 303 || x.status === 302, String(x.status));
    }
  }

  // ── alice reads her mail ──────────────────────────────────────────────────
  section('Reading');
  const actx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US', acceptDownloads: true });
  const page = await actx.newPage();
  const problems = watch(page);
  const sockets = [];
  page.on('websocket', (ws) => sockets.push(ws.url()));
  await login(page, server.url, ALICE);
  await page.getByRole('link', { name: 'Mail', exact: true }).first().click();
  await page.waitForURL('**/mail**');
  check('the Mail link opens the inbox', /folder=INBOX/.test(page.url()) || /\/mail/.test(page.url()));
  const rows = page.locator('#mail-list .rm-row');
  const arrived = await eventually(async () => {
    await page.goto(server.url + '/mail');
    return (await rows.count()) >= 2;
  }, 60);
  check('both delivered messages are listed', arrived, `rows=${await rows.count()}`);
  check('they are shown as unread', (await page.locator('#mail-list .rm-row.unread').count()) === 2);
  check('the folder list shows the unread count', await page.locator('#mail-folders .badge').first().isVisible());
  check('standard folders are present', (await page.locator('#mail-folders a').allInnerTexts()).join('|').includes('Sent'));
  await page.screenshot({ path: `${SHOTS}/01-inbox.png` });

  // The plain message.
  await rows.filter({ hasText: 'Lunch on Thursday' }).click();
  await page.locator('#mail-preview h1', { hasText: 'Lunch on Thursday' }).waitFor();
  check('opening a message shows it in the preview pane', await page.locator('#mail-preview .rm-message-text').innerText().then((t) => t.includes('pizza on Thursday')));
  check('the URL follows the selected message', /open=\d+/.test(page.url()), page.url());
  check('opening marks it read', await eventually(async () => (await page.locator('#mail-list .rm-row.unread').count()) === 1, 15));

  // The hostile HTML message.
  await rows.filter({ hasText: 'Weekly report' }).click();
  const frameEl = page.locator('#mail-preview iframe.rm-mail-frame');
  await frameEl.waitFor();
  check('HTML mail is shown in a sandboxed iframe', ((await frameEl.getAttribute('sandbox')) || '').split(' ').every((f) => ['allow-popups', 'allow-popups-to-escape-sandbox'].includes(f)));
  const frame = await (await frameEl.elementHandle()).contentFrame();
  await frame.waitForLoadState();
  check('the formatted content is displayed', (await frame.locator('h2').innerText()) === 'Quarterly numbers');
  check('scripts did not run', (await page.evaluate(() => window.__pwned)) === undefined && (await page.title()).indexOf('pwned') === -1);
  check('no script, iframe, handler or javascript: link is left', (await frame.locator('script, iframe, object, embed, form').count()) === 0
    && (await frame.locator('[onerror], [onclick]').count()) === 0
    && (await frame.locator('a[href^="javascript:" i]').count()) === 0);
  check('remote pictures are blocked by default', (await frame.locator('img[src*="tracker.example"]').count()) === 0);
  check('the user is told, and can opt in', await page.getByText('Pictures from the internet are hidden').isVisible());
  check('links open in a new window without leaking the page', (await frame.locator('a[href="https://example.com/report"]').getAttribute('rel')).includes('noreferrer'));
  const headers = (await page.request.get(await frameEl.getAttribute('src').then((s) => new URL(s, server.url).toString()))).headers();
  check('the body document has a locked-down CSP', /sandbox/.test(headers['content-security-policy'] || '') && /default-src 'none'/.test(headers['content-security-policy'] || '') && /img-src 'none'/.test(headers['content-security-policy'] || ''), headers['content-security-policy']);
  await page.screenshot({ path: `${SHOTS}/02-html-message.png` });
  await page.getByRole('link', { name: 'Show pictures' }).click();
  await page.waitForFunction(() => (document.querySelector('#mail-preview iframe') || {}).src?.includes('images=1'));
  const frame2 = await (await page.locator('#mail-preview iframe.rm-mail-frame').elementHandle()).contentFrame();
  await frame2.waitForLoadState();
  check('"Show pictures" lets the remote image through', (await frame2.locator('img[src*="tracker.example"]').count()) === 1);

  // Attachment download.
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('link', { name: 'notes.txt' }).click()]);
  const dpath = path.join(tmp, 'dl.txt');
  await download.saveAs(dpath);
  check('the attachment downloads with its name and content', download.suggestedFilename() === 'notes.txt' && readFileSync(dpath, 'utf8') === 'attachment content: lasagna recipe');

  // Things that are not there.
  const gone = await page.request.get(`${server.url}/mail/message/99999/body?folder=INBOX`);
  check('a message that does not exist is a 404', gone.status() === 404, String(gone.status()));
  const badFolder = await page.request.get(`${server.url}/mail?folder=${encodeURIComponent('../../etc')}`);
  check('a folder that does not exist is a 404, not an error page with details', badFolder.status() === 404, String(badFolder.status()));
  const ctl = await page.request.get(`${server.url}/mail?folder=${encodeURIComponent('a\r\nb')}`);
  check('a folder name with control characters is refused', ctl.status() === 400, String(ctl.status()));

  // ── reply and send ────────────────────────────────────────────────────────
  section('Reply and send');
  await rows.filter({ hasText: 'Lunch on Thursday' }).click();
  await page.locator('#mail-preview h1', { hasText: 'Lunch on Thursday' }).waitFor();
  await page.getByRole('link', { name: 'Reply', exact: true }).click();
  await page.waitForURL('**/mail/compose**');
  check('the reply is addressed to the sender', (await page.locator('#c-to').inputValue()).includes('outsider@example.org'));
  check('the subject gets "Re:"', (await page.locator('#c-subject').inputValue()) === 'Re: Lunch on Thursday');
  check('the original is quoted', (await page.locator('#c-body').inputValue()).includes('> are you free for pizza on Thursday?'));
  check('the sender is the signed-in user and cannot be edited', (await page.locator('#c-from').getAttribute('readonly')) !== null && (await page.locator('#c-from').inputValue()).includes(ALICE));
  const replySubject = `Re: Lunch on Thursday`;
  await page.locator('#c-body').fill('Yes, Thursday works.' + (await page.locator('#c-body').inputValue()));
  await page.screenshot({ path: `${SHOTS}/03-compose.png` });
  await page.getByRole('button', { name: 'Send' }).click();
  await page.waitForURL('**done=sent**');
  check('after sending you land on the inbox with a confirmation', await page.getByRole('status').filter({ hasText: 'Your message was sent' }).isVisible());
  check('the reply reaches the outside world (Mailpit)', await eventually(() => mailpitHas(replySubject), 30));

  // A message to bob with an attachment.
  await page.goto(server.url + '/mail/compose');
  await page.locator('#c-to').fill(`Bob Baker <${BOB}>`);
  await page.getByRole('button', { name: 'Add Cc' }).click();
  check('Add Cc reveals the field', await page.locator('#c-cc').isVisible());
  await page.locator('#c-subject').fill('Plans for Friday');
  await page.locator('#c-body').fill('Hello Bob, the plan is in the attachment. Greetings from Alice.');
  writeFileSync(path.join(tmp, 'plan.txt'), 'plan: climbing at dawn');
  await page.locator('#c-files').setInputFiles(path.join(tmp, 'plan.txt'));
  await page.getByRole('button', { name: 'Send' }).click();
  await page.waitForURL('**done=sent**');
  // Invalid input is explained and nothing is lost.
  await page.goto(server.url + '/mail/compose');
  await page.locator('#c-to').fill('not-an-address');
  await page.locator('#c-subject').fill('Keep me');
  const bad = await act(page, '/mail/send', () => page.getByRole('button', { name: 'Send' }).click());
  check('an invalid address is refused with 422', bad.status() === 422, String(bad.status()));
  check('an invalid address is explained next to the form', await page.locator('#compose-status .alert-danger').isVisible());
  check('…and the form keeps what was typed', (await page.locator('#c-subject').inputValue()) === 'Keep me');
  const csrf = (await actx.cookies()).find((c) => c.name === 'rm_csrf').value;
  for (const [field, value] of [['to', 'a@b.co\r\nBcc: victim@example.net'], ['subject', 'hi\r\nBcc: victim@example.net']]) {
    const r = await page.request.post(server.url + '/mail/send', {
      headers: { 'x-csrf-token': csrf },
      multipart: { to: field === 'to' ? value : 'a@b.co', subject: field === 'subject' ? value : 's', body: 'b' },
    });
    check(`a line break in the ${field} field is refused by the server`, r.status() === 422, String(r.status()));
  }

  // Sent copy.
  await page.goto(server.url + '/mail?folder=Sent');
  check('sent mail is kept in the Sent folder', await eventually(async () => { await page.goto(server.url + '/mail?folder=Sent'); return (await page.locator('#mail-list .rm-row', { hasText: 'Plans for Friday' }).count()) === 1; }, 20));
  await page.locator('#mail-list .rm-row', { hasText: 'Plans for Friday' }).click();
  check('the Sent copy has the attachment', await page.getByRole('link', { name: 'plan.txt' }).waitFor({ timeout: 10000 }).then(() => true, () => false));

  // ── bob receives ──────────────────────────────────────────────────────────
  section('The recipient');
  const bctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'en-US', acceptDownloads: true });
  const bpage = await bctx.newPage();
  const bproblems = watch(bpage);
  await login(bpage, server.url, BOB);
  const got = await eventually(async () => { await bpage.goto(server.url + '/mail'); return (await bpage.locator('#mail-list .rm-row', { hasText: 'Plans for Friday' }).count()) === 1; }, 60);
  check('bob receives alice\'s message', got);
  await bpage.locator('#mail-list .rm-row', { hasText: 'Plans for Friday' }).click();
  await bpage.locator('#mail-preview h1', { hasText: 'Plans for Friday' }).waitFor();
  check('it shows alice\'s name and address', (await bpage.locator('#mail-preview dl').innerText()).includes('Alice Anders') && (await bpage.locator('#mail-preview dl').innerText()).includes(ALICE));
  const [bdl] = await Promise.all([bpage.waitForEvent('download'), bpage.getByRole('link', { name: 'plan.txt' }).click()]);
  await bdl.saveAs(path.join(tmp, 'bob.txt'));
  check('bob can download the attachment', readFileSync(path.join(tmp, 'bob.txt'), 'utf8') === 'plan: climbing at dawn');
  check('bob does not see alice\'s mail', (await bpage.locator('#mail-list .rm-row', { hasText: 'Lunch on Thursday' }).count()) === 0);

  // ── live updates and keyboard ─────────────────────────────────────────────
  section('Live updates and keyboard shortcuts');
  await page.goto(server.url + '/mail');
  check('the page opens a WebSocket for new mail', await eventually(() => sockets.length > 0, 10));
  const before = await rows.count();
  deliver(ALICE, `From: Late Sender <late@example.org>\r\nTo: ${ALICE}\r\nSubject: Fresh arrival ${RUN}\r\nMessage-ID: <fresh-${RUN}@example.org>\r\n\r\nJust arrived.\r\n`);
  check('new mail appears without reloading the page', await eventually(async () => (await page.locator('#mail-list .rm-row', { hasText: `Fresh arrival ${RUN}` }).count()) === 1, 40), `before=${before}`);
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('j');
  await page.locator('#mail-preview article').waitFor();
  const first = await page.locator('#mail-preview h1').innerText();
  await page.keyboard.press('j');
  await eventually(async () => (await page.locator('#mail-preview h1').innerText()) !== first, 10);
  check('j moves to the next message', (await page.locator('#mail-preview h1').innerText()) !== first);
  await page.keyboard.press('k');
  await eventually(async () => (await page.locator('#mail-preview h1').innerText()) === first, 10);
  check('k moves back', (await page.locator('#mail-preview h1').innerText()) === first);
  await page.keyboard.press('?');
  check('? shows the shortcut help', await page.locator('#mail-help').isVisible());
  await page.keyboard.press('?');
  await page.keyboard.press('/');
  check('/ focuses the search box', await page.evaluate(() => document.activeElement && document.activeElement.id === 'mail-search'));
  await page.keyboard.type('j');
  check('typing in the search box does not trigger shortcuts', (await page.locator('#mail-search').inputValue()) === 'j');
  await page.locator('#mail-search').fill('');
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('c');
  await page.waitForURL('**/mail/compose');
  check('c opens the compose page', true);

  // ── search ────────────────────────────────────────────────────────────────
  section('Search');
  const found = await eventually(async () => {
    await page.goto(`${server.url}/mail?q=${encodeURIComponent('pizza')}`);
    return (await page.locator('#mail-list .rm-row', { hasText: 'Lunch on Thursday' }).count()) === 1;
  }, 60);
  check('searching finds a message by a word in its text (Postgres full-text)', found);
  await page.goto(`${server.url}/mail?q=${encodeURIComponent('news@example.org')}`);
  check('searching by an address finds its messages', await eventually(async () => { await page.goto(`${server.url}/mail?q=${encodeURIComponent('news@example.org')}`); return (await page.locator('#mail-list .rm-row', { hasText: 'Weekly report' }).count()) === 1; }, 20));
  await page.goto(`${server.url}/mail?q=${encodeURIComponent("'; DROP TABLE messages_meta; --")}`);
  check('hostile search text is harmless', (await page.locator('#mail-list').count()) === 1);
  check('the index tables are intact', Number(psql('SELECT count(*) FROM messages_meta').stdout.trim()) > 0);
  await page.goto(`${server.url}/mail?q=${encodeURIComponent('zzzznotfound')}`);
  check('no results says so', await page.getByText('Nothing found').isVisible());

  // ── folders, move, spam, delete ───────────────────────────────────────────
  section('Folders, spam and deleting');
  await page.goto(server.url + '/mail/folders');
  await page.locator('#new-folder').fill('Projekte Ü & Co');
  await page.getByRole('button', { name: 'Create folder' }).click();
  await page.waitForURL('**done=created**');
  check('a folder with spaces, umlauts and & can be created', await page.getByRole('cell', { name: 'Projekte Ü & Co' }).or(page.getByRole('rowheader', { name: 'Projekte Ü & Co' })).isVisible());
  await page.locator('#new-folder').fill('Projekte Ü & Co');
  const twice = await act(page, '/mail/folders/create', () => page.getByRole('button', { name: 'Create folder' }).click());
  check('creating it twice is refused with an explanation', twice.status() === 422 && await page.locator('#folder-status .alert-danger').isVisible(), String(twice.status()));
  await page.locator('#new-folder').fill('../escape');
  const dots = await act(page, '/mail/folders/create', () => page.getByRole('button', { name: 'Create folder' }).click());
  check('a name with ".." is refused', dots.status() === 422, String(dots.status()));
  await page.goto(server.url + '/mail');
  await rows.filter({ hasText: 'Lunch on Thursday' }).click();
  await page.locator('#mail-preview h1', { hasText: 'Lunch on Thursday' }).waitFor();
  await page.locator('#move-target').selectOption({ label: 'Projekte Ü & Co' });
  await act(page, '/move', () => page.getByRole('button', { name: 'Move', exact: true }).click());
  check('moving a message removes it from the inbox', await eventually(async () => (await rows.filter({ hasText: 'Lunch on Thursday' }).count()) === 0, 15));
  await page.goto(`${server.url}/mail?folder=${encodeURIComponent('Projekte Ü & Co')}`);
  check('…and it is in the new folder', (await page.locator('#mail-list .rm-row', { hasText: 'Lunch on Thursday' }).count()) === 1);
  // Spam goes to Junk and back.
  await page.goto(server.url + '/mail');
  await rows.filter({ hasText: 'Weekly report' }).click();
  await page.locator('#mail-preview h1', { hasText: 'Weekly report' }).waitFor();
  await act(page, '/spam', () => page.locator('#mail-preview').getByRole('button', { name: 'Spam', exact: true }).click());
  check('"Spam" moves it out of the inbox', await eventually(async () => (await rows.filter({ hasText: 'Weekly report' }).count()) === 0, 15));
  await page.goto(server.url + '/mail?folder=Junk');
  check('…into Junk', (await page.locator('#mail-list .rm-row', { hasText: 'Weekly report' }).count()) === 1);
  await page.locator('#mail-list .rm-row', { hasText: 'Weekly report' }).click();
  await act(page, '/spam', () => page.locator('#mail-preview').getByRole('button', { name: 'Not spam' }).click());
  check('"Not spam" brings it back', await eventually(async () => { await page.goto(server.url + '/mail'); return (await rows.filter({ hasText: 'Weekly report' }).count()) === 1; }, 15));
  // Delete: Trash, then for good.
  await rows.filter({ hasText: 'Weekly report' }).click();
  await page.locator('#mail-preview h1', { hasText: 'Weekly report' }).waitFor();
  await act(page, '/delete', () => page.locator('#mail-preview').getByRole('button', { name: 'Delete' }).click());
  await page.goto(server.url + '/mail?folder=Trash');
  check('deleting moves a message to Trash', (await page.locator('#mail-list .rm-row', { hasText: 'Weekly report' }).count()) === 1);
  await page.locator('#mail-list .rm-row', { hasText: 'Weekly report' }).click();
  await page.locator('#mail-preview h1', { hasText: 'Weekly report' }).waitFor();
  await act(page, '/delete', () => page.locator('#mail-preview').getByRole('button', { name: 'Delete' }).click());
  check('deleting from Trash removes it for good', await eventually(async () => { await page.goto(server.url + '/mail?folder=Trash'); return (await page.locator('#mail-list .rm-row', { hasText: 'Weekly report' }).count()) === 0; }, 15));
  // Rename and delete the folder.
  await page.goto(server.url + '/mail/folders');
  const row = page.locator('tr', { hasText: 'Projekte Ü & Co' });
  await row.getByPlaceholder('New name').fill('Archiv 2025');
  await row.getByRole('button', { name: 'Rename' }).click();
  await page.waitForURL('**done=renamed**');
  check('a folder can be renamed', await page.getByRole('rowheader', { name: 'Archiv 2025' }).isVisible());
  page.once('dialog', (d) => d.accept());
  await page.locator('tr', { hasText: 'Archiv 2025' }).getByRole('button', { name: 'Delete' }).click();
  await page.waitForURL('**done=deleted**');
  check('…and deleted after confirmation', (await page.getByText('Archiv 2025').count()) === 0);
  check('standard folders have no rename or delete buttons', (await page.locator('tr', { hasText: 'Trash' }).getByRole('button').count()) === 0);

  // ── German ────────────────────────────────────────────────────────────────
  section('Localisation');
  const gctx = await browser.newContext({ locale: 'de-DE' });
  const gpage = await gctx.newPage();
  await login(gpage, server.url, ALICE);
  await gpage.goto(server.url + '/mail');
  check('the mail UI follows the browser language', await gpage.getByText('Verfassen').first().isVisible() && await gpage.locator('#mail-folders').getByText('Posteingang').isVisible());
  await gctx.close();

  // ── WebSocket origin check ────────────────────────────────────────────────
  section('WebSocket safety');
  const cookies = (await actx.cookies()).map((c) => `${c.name}=${c.value}`).join('; ');
  const upgrade = (origin) => new Promise((resolve) => {
    const u = new URL(server.url);
    const req = net.connect(Number(u.port), u.hostname, () => {
      req.write(`GET /mail/ws HTTP/1.1\r\nHost: ${u.host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n${origin ? `Origin: ${origin}\r\n` : ''}Cookie: ${cookies}\r\n\r\n`);
    });
    let buf = '';
    req.on('data', (d) => { buf += d; if (buf.includes('\r\n')) { req.destroy(); resolve(buf.split('\r\n')[0]); } });
    req.on('error', () => resolve('error'));
    setTimeout(() => { req.destroy(); resolve('timeout'); }, 5000);
  });
  check('a WebSocket from another site is refused', /403/.test(await upgrade('https://evil.example')));
  check('a WebSocket without an Origin is refused', /403/.test(await upgrade(null)));
  check('a same-site WebSocket is accepted', /101/.test(await upgrade(server.url)));

  check('no console errors or CSP violations for alice', problems.length === 0, problems.join(' | '));
  check('no console errors or CSP violations for bob', bproblems.length === 0, bproblems.join(' | '));
  await actx.close();
  await bctx.close();
} catch (error) {
  console.log(`\nSCRIPT ERROR: ${error.stack || error}`);
  failures.push(`script error: ${error.message}`);
} finally {
  await browser.close();
  server.stop();
  rmSync(tmp, { recursive: true, force: true });
  // Remove everything this run created.
  psql(`DELETE FROM users WHERE email LIKE '%@${DOMAIN}'`);
  psql(`DELETE FROM domains WHERE name = '${DOMAIN}'`);
  rmSync(path.join(VMAIL, DOMAIN), { recursive: true, force: true });
}
console.log(`\n${pass} checks passed, ${failures.length} failed`);
if (failures.length) console.log('FAILED:\n - ' + failures.join('\n - '));
console.log(`screenshots: ${SHOTS}`);
console.log(failures.length ? 'WEBMAIL_RESULT=FAIL' : 'WEBMAIL_RESULT=PASS');
process.exit(failures.length ? 1 : 0);
