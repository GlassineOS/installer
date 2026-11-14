// SPDX-License-Identifier: GPL-2.0-or-later
// The screens in headless Chromium: every screen, in each of its states, with every plan golden
// file of the core (and the Check screen with every check report), renders without a console
// error, a failed request or a request to another site, and shows what its record says. Then a
// few interactions. Needs Chromium (CHROMIUM=path; make docker-web-check provides it).
//
//   node web/test/screens.js [--shots DIR] [--only SCREEN]
'use strict';
const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const puppeteer = require('puppeteer-core');
const { serve } = require('./serve.js');

const ROOT = path.resolve(__dirname, '..', '..');
const GOLDEN = fs.readdirSync(path.join(ROOT, 'core/tests/golden')).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort();
const REPORTS = fs.readdirSync(path.join(ROOT, 'web/fixtures/reports')).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort();
const plans = Object.fromEntries(GOLDEN.map((g) => [g, JSON.parse(fs.readFileSync(path.join(ROOT, 'core/tests/golden', g + '.json'), 'utf8'))]));

// Every screen and the states it has (undefined: the default).
const SCREENS = {
  start: [undefined, 'live'],
  reach: [undefined, 'hand', 'debugging', 'chooser', 'waiting', 'found', 'notfresh'],
  check: [undefined],
  plan: [undefined],
  supply: [undefined, 'nofile'],
  confirm: [undefined, 'noroom'],
  backup: ['saving', 'verifying', 'verified', 'failed', 'refused'],
  install: ['midway', 'ponr', 'stopping', 'stopped'],
  verify: ['waiting', 'notaccepted'],
  resume: [undefined],
  installed: [undefined],
  done: ['installed', 'stock'],
};
// Screens that show the plan's steps, and in which form.
const STEP_LISTS = { plan: true, confirm: true, install: true };

const args = process.argv.slice(2);
const opt = (k) => { const i = args.indexOf(k); return i >= 0 ? args[i + 1] : null; };
const SHOTS = opt('--shots'), ONLY = opt('--only');

// Words a person must never see bare: the record's ids with a hyphen (step ids, escapes,
// capabilities, hardware, statuses). Inside <details> they are allowed (innerText skips a closed one).
function bareIds(plan) {
  const ids = new Set(['previous-system-starts', 'glassine-starts', 'installer-can-finish', 'installer-can-undo', 'special-hardware',
    'needs-answers', 'developer-only', 'emergency-cable', 'start-kernel', 'fallback-boot', 'trial-boot', 'keep-stock', 'secondary-recovery', 'choose-at-start',
    'emergency-programmer', 'fits-unknown', 'wrong-chip-or-key', 'known-bad', 'not-a-programmer', 'emergency-download']);
  if (plan) for (const s of plan.steps) if (s.id.includes('-')) ids.add(s.id);
  return [...ids];
}

// The synthetic files of the core's tests (web/fixtures/supplied/*.hex), written back as files
// for the browser's file window, in a directory of their own.
function suppliedFiles() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'glassine-supplied-'));
  const src = path.join(ROOT, 'web/fixtures/supplied');
  for (const f of fs.readdirSync(src).filter((x) => x.endsWith('.hex'))) {
    fs.writeFileSync(path.join(dir, f.replace(/\.hex$/, '.bin')), Buffer.from(fs.readFileSync(path.join(src, f), 'utf8').replace(/\s/g, ''), 'hex'));
  }
  return dir;
}

async function main() {
  const server = await serve(0);
  // The screens with the core's test records (?fixtures=1); live.js runs them with the core itself.
  const base = `http://127.0.0.1:${server.address().port}/web/?fixtures=1`;
  const exe = process.env.CHROMIUM || '/usr/bin/chromium';
  const noSandbox = process.env.WEB_NO_SANDBOX === '1' || process.getuid?.() === 0;
  const browser = await puppeteer.launch({ executablePath: exe, headless: true, args: [...(noSandbox ? ['--no-sandbox'] : []), '--disable-gpu', '--font-render-hinting=none'] });
  let failures = 0, passes = 0;
  const test = async (name, fn) => {
    try { await fn(); passes++; process.stdout.write(`ok - ${name}\n`); } catch (e) { failures++; process.stdout.write(`not ok - ${name}\n  ${String((e && e.stack) || e).split('\n').slice(0, 6).join('\n  ')}\n`); }
  };

  // A page that records everything that went wrong on it.
  async function open({ noUsb = false } = {}) {
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 1000 });
    const problems = [];
    await page.setRequestInterception(true);
    page.on('request', (r) => {
      const u = new URL(r.url());
      // Nothing is ever sent anywhere: a picked file stays in the page (spec 004 FR-003).
      if (r.method() !== 'GET') problems.push(`a ${r.method()} request: ${r.url()}`);
      if (u.protocol === 'data:' || u.protocol === 'blob:' || (u.hostname === '127.0.0.1' && u.port === String(server.address().port))) r.continue();
      else { problems.push(`request to another site: ${r.url()}`); r.abort(); }
    });
    page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warn') problems.push(`console ${m.type()}: ${m.text()}`); });
    page.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
    page.on('requestfailed', (r) => { if (!/^blob:/.test(r.url())) problems.push(`request failed: ${r.url()} ${r.failure() && r.failure().errorText}`); });
    page.on('response', (r) => { if (r.status() >= 400) problems.push(`HTTP ${r.status()}: ${r.url()}`); });
    if (noUsb) await page.evaluateOnNewDocument(() => { delete Navigator.prototype.usb; });
    return { page, problems };
  }
  const ready = (page) => page.waitForFunction(() => document.documentElement.dataset.ready, { timeout: 15000 });
  // Move to an address and wait until that address has rendered.
  async function go(page, hash) {
    const want = await page.evaluate((h) => { location.hash = h; return location.hash; }, hash);
    await page.waitForFunction((w) => document.documentElement.dataset.hash === w && document.documentElement.dataset.ready, { timeout: 15000 }, want);
  }

  const { page, problems } = await open();
  // Every <details> open, for what is folded away (innerText skips a closed one).
  const unfold = (p = page) => p.evaluate(() => { for (const d of document.querySelectorAll('details')) d.open = true; });
  const SUPPLIED = path.join(ROOT, 'web/fixtures/supplied');
  const supplied = (n) => JSON.parse(fs.readFileSync(path.join(SUPPLIED, n + '.json'), 'utf8'));
  const FILES = suppliedFiles();
  // Picks a file on Supply, as the browser's file window would, and waits for the core's check.
  const pickFile = async (name) => {
    const input = await page.$('#file-input');
    await input.uploadFile(path.join(FILES, name + '.bin'));
    await page.waitForFunction((v) => { const f = document.querySelector('#file'); return f && f.dataset.verdict === v; }, { timeout: 10000 }, name);
  };
  await page.goto(base, { waitUntil: 'load' });
  await ready(page);

  await test('headless Chromium has WebUSB (else every screen would show the other-browser message)', async () => {
    assert.strictEqual(await page.evaluate(() => 'usb' in navigator), true);
  });

  const checkPage = async (route, hash, plan, label) => {
    problems.length = 0;
    await go(page, hash);
    const got = await page.evaluate(() => ({
      ready: document.documentElement.dataset.ready,
      h1: (document.querySelector('h1') || {}).textContent || '',
      text: document.body.innerText,
      chip: (document.querySelector('.ins-head__machine') || {}).textContent || '',
      // the step list itself, not the one folded under "If a step goes wrong"
      steps: document.querySelectorAll('li[data-step]:not(details li)').length,
      owed: document.querySelectorAll('[data-owed]').length,
    }));
    assert.strictEqual(got.ready, route, `rendered ${got.ready}, not ${route}`);
    assert.ok(got.h1.trim(), 'no title');
    assert.deepStrictEqual(problems, []);
    for (const bad of ['undefined', 'null', '[object Object]', 'NaN']) assert.ok(!new RegExp(`\\b${bad.replace(/[[\]]/g, '\\$&')}\\b`).test(got.text), `“${bad}” on the page`);
    for (const id of bareIds(plan)) assert.ok(!got.text.includes(id), `the id ${id} is shown without words`);
    if (plan && route !== 'start') {
      const name = plan.description === 'uefi-pc' ? 'This PC' : plan.device;
      if (route !== 'reach' || /found|notfresh/.test(hash) || plan.description === 'uefi-pc') assert.ok(got.chip.includes(name), `the header doesn’t name ${name}: “${got.chip}”`);
    }
    if (plan && STEP_LISTS[route] && !(route === 'plan' && plan.status === 'refused')) assert.strictEqual(got.steps, plan.steps.length, `${got.steps} steps shown, the plan has ${plan.steps.length}`);
    if (SHOTS) {
      fs.mkdirSync(SHOTS, { recursive: true });
      await page.screenshot({ path: path.join(SHOTS, `${label}.png`), fullPage: true });
    }
    return got;
  };

  for (const [route, states] of Object.entries(SCREENS)) {
    if (ONLY && route !== ONLY) continue;
    for (const g of GOLDEN) {
      for (const st of states) {
        const label = `${route}${st ? '.' + st : ''}--${g}`;
        await test(`${route}${st ? ' (' + st + ')' : ''} with ${g}`, () => checkPage(route, `#/${route}?plan=${g}${st ? '&state=' + st : ''}`, plans[g], label));
      }
    }
  }

  if (!ONLY || ONLY === 'check') {
    for (const r of REPORTS) {
      await test(`check with the report of ${r}`, async () => {
        const got = await checkPage('check', `#/check?machine=${r}`, null, `check--report-${r}`);
        const rep = JSON.parse(fs.readFileSync(path.join(ROOT, 'web/fixtures/reports', r + '.json'), 'utf8'));
        const want = rep.supported ? /can run GlassineOS|^This is /m
          : rep.recognition.result === 'none' ? /doesn’t support this device/ : rep.recognition.result === 'conflict' ? /doesn’t match its description/ : /restarted to be recognised|can’t tell/;
        assert.match(got.text, want);
        // only a description that matched names the machine
        if (rep.recognition.result !== 'match') for (const d of ['Onyx Boox Note Air']) assert.ok(!got.chip.includes(d) && !got.text.includes('the ' + d), `names ${d}, which it didn’t match`);
      });
    }
  }

  await test('a browser without WebUSB gets the Chromium message on every screen', async () => {
    const o = await open({ noUsb: true });
    for (const h of ['#/start', '#/plan?plan=noteair-cable-yes']) {
      await o.page.goto(base + h, { waitUntil: 'load' });
      await o.page.waitForSelector('#nowebusb', { timeout: 10000 });
      assert.match(await o.page.evaluate(() => document.body.innerText), /Chromium-based browser/);
    }
    assert.deepStrictEqual(o.problems, []);
    await o.page.close();
  });

  await test('answering the plan’s question re-plans with the core’s record', async () => {
    await go(page, '#/plan?plan=noteair-keep-stock');
    assert.ok(await page.$('#needs-answers'));
    assert.ok(await page.$eval('#confirm', (b) => b.disabled));
    const answer = async (v, rendered) => {
      await page.evaluate((i) => document.querySelectorAll('input[name="hw-emergency-cable"]')[i].click(), v === 'yes' ? 0 : 1);
      await page.waitForFunction((w) => document.documentElement.dataset.hash.includes(w) && document.documentElement.dataset.hash === location.hash, { timeout: 10000 }, `hardware.emergency-cable=${v}`);
      await page.waitForSelector(rendered, { timeout: 10000 });
    };
    await answer('yes', '#developer-only');
    await answer('no', '#no-record'); // "no" without developer=1 was never planned
  });

  await test('a refused plan stops: no steps, Continue off', async () => {
    await go(page, '#/plan?plan=noteair-cable-no');
    assert.ok(await page.$('#refused'));
    assert.ok(await page.$eval('#confirm', (b) => b.disabled));
    assert.strictEqual(await page.$$eval('li[data-step]', (x) => x.length), 0);
  });

  await test('confirm: the erase tick and the replaced system’s name gate the install', async () => {
    // the backup needs the file the person supplies: a known one, checked on Supply
    await go(page, '#/supply?plan=noteair-no-stock-developer');
    await pickFile('known');
    await go(page, '#/confirm?plan=noteair-no-stock-developer');
    const off = () => page.$eval('#install', (b) => b.tagName === 'BUTTON' && b.disabled);
    assert.ok(await off());
    await page.click('#erase-ack');
    assert.ok(await off(), 'enabled before the name was typed');
    await page.type('#osname', 'stock system');
    await page.waitForFunction(() => document.querySelector('#install').tagName === 'A', { timeout: 5000 });
    await go(page, '#/confirm?plan=pc-windows-replace');
    assert.strictEqual(await page.$('#erase-ack'), null, 'a replaced system’s files are confirmed by its name, not a tick');
    await page.type('#osname', 'Windows');
    await page.waitForFunction(() => document.querySelector('#install').tagName === 'A', { timeout: 5000 });
  });

  await test('a stop waits for the plan’s next safe stop', async () => {
    const plan = plans['pc-windows-replace'];
    const i = plan.steps.findIndex((x) => x.point_of_no_return);
    assert.strictEqual(plan.steps[i].safe_stop, false, 'the fixture no longer has an unsafe step');
    await go(page, `#/install?plan=pc-windows-replace&state=ponr`);
    const t = await page.evaluate(() => document.body.innerText);
    assert.ok(!/after this step/.test(t), 'offers to stop after a step that isn’t a safe stop');
    assert.match(t, new RegExp(`after step ${i + 2},`));
  });

  const pick = async (name, value, want, rendered) => {
    await page.evaluate((n, v) => document.querySelector(`input[name="${n}"][value="${v}"]`).click(), name, value);
    await page.waitForFunction((w) => document.documentElement.dataset.hash.includes(w) && document.documentElement.dataset.hash === location.hash, { timeout: 10000 }, want);
    await page.waitForSelector(rendered, { timeout: 10000 });
  };

  await test('the backup: sizes from each disk image', async () => {
    await go(page, '#/plan?plan=noteair-no-stock-developer');
    await unfold();
    const gib = (plans['noteair-no-stock-developer'].backup.images[0].estimate_bytes / 2 ** 30).toFixed(1);
    assert.ok((await page.$eval('#backup-images', (x) => x.innerText)).includes(`Disk 0: about ${gib} GiB.`), 'the image’s size isn’t the plan’s');
    await go(page, '#/plan?plan=pc-two-disks-first');
    await unfold();
    const t = await page.$eval('#backup-images', (x) => x.innerText);
    assert.match(t, /Disk 0, NVME: at most 512\.0 GiB, the disk’s size\./);
    assert.ok(!/Disk 1/.test(t), 'images a disk the plan doesn’t write to');
    await go(page, '#/backup?plan=pc-two-disks-second');
    assert.match(await page.evaluate(() => document.body.innerText), /Disk 1, SATA, whole/);
  });

  await test('the backup can be switched off, with its consequence, and on again', async () => {
    await go(page, '#/plan?plan=pc-windows-replace');
    assert.strictEqual(await page.$('#backup-off'), null);
    await pick('backup', 'off', 'choice.backup=off', '#backup-off');
    assert.ok(await page.$eval('#choices', (d) => d.open), 'the choices stay folded with the backup off');
    await unfold();
    const t = await page.evaluate(() => document.body.innerText);
    assert.match(t, /no way back to the maker’s system|no way back to the maker's system/);
    assert.match(t, /step 2 can’t be undone/, 'doesn’t name the step whose undo is lost');
    assert.match(t, /The installer can’t undo it: the backup is off\./);
    assert.ok(!(await page.$eval('#confirm', (b) => b.disabled)), 'the plan without a backup is ready');
    await page.click('#confirm');
    await page.waitForFunction(() => document.documentElement.dataset.hash.startsWith('#/confirm'), { timeout: 10000 });
    await page.waitForSelector('#backup-off', { timeout: 10000 });
    assert.strictEqual(await page.$('input[name="backup-to"]'), null, 'offers a place for a backup that is off');
    await go(page, '#/install?plan=pc-windows-replace-no-backup&state=stopped');
    assert.ok(!/Undo/.test(await page.evaluate(() => document.body.innerText)), 'offers an undo without a backup');
    await go(page, '#/plan?plan=pc-windows-replace-no-backup');
    await pick('backup', 'full', 'choice.backup=full', '#backup-images');
  });

  await test('a least size no release states yet is shown as not known yet', async () => {
    const rows = async () => page.evaluate(() => document.body.innerText);
    await go(page, '#/plan?plan=pc-small-disk-no-release');
    assert.match(await rows(), /On disk 0\s+1\.0 GiB and GlassineOS’s system partition, whose size is not known yet, of 6\.0 GiB free/);
    await go(page, '#/plan?plan=noteair-keep-stock');
    assert.match(await rows(), /GlassineOS’s system partition\s+3\.0 GiB; what this release needs there is not known yet/);
    assert.ok(!/null|undefined|NaN/.test(await rows()), 'a size that isn’t known shows as a word');
    // A release that states more than the fixed partition holds: refused, with the core's reason.
    await go(page, '#/plan?plan=noteair-release-too-big');
    assert.match(await page.$eval('#refused', (x) => x.textContent), /System_b is too small for this release: its base needs at least 8\.0 GiB there and it is 3\.0 GiB/);
  });

  await test('the disk GlassineOS goes on: each fixed disk, and the plan of the one chosen', async () => {
    await go(page, '#/plan?plan=pc-two-disks-first');
    const disks = () => page.$$eval('input[name="choice-target-disk"]', (xs) => xs.map((x) => x.value + (x.checked ? '*' : '')));
    assert.deepStrictEqual(await disks(), ['first*', 'disk1']);
    await pick('choice-target-disk', 'disk1', 'choice.target-disk=disk1', '#backup-images');
    await unfold();
    assert.deepStrictEqual(await disks(), ['first', 'disk1*']);
    assert.match(await page.$eval('#backup-images', (x) => x.innerText), /Disk 1/);
  });

  await test('the backup place: the plan’s place and each disk it doesn’t image', async () => {
    const places = () => page.$$eval('input[name="backup-to"]', (xs) => xs.map((x) => x.value + (x.checked ? '*' : '')));
    await go(page, '#/confirm?plan=pc-two-disks-first');
    assert.deepStrictEqual(await places(), ['installer-media*', 'disk1', 'disk2']);
    await go(page, '#/confirm?plan=pc-two-disks-second');
    assert.deepStrictEqual(await places(), ['installer-media*', 'disk0', 'disk2']);
    await go(page, '#/confirm?plan=noteair-no-stock-developer');
    assert.deepStrictEqual(await places(), ['computer*'], 'offers the device’s own storage for its backup');
    await go(page, '#/confirm?plan=pc-two-disks-first');
    await pick('backup-to', 'disk1', 'choice.backup-to=disk1', '#no-record'); // planned by the core's tests, but without a golden file
  });

  await test('the plan’s escapes come with the description’s words', async () => {
    await go(page, '#/plan?plan=noteair-cable-yes');
    await unfold();
    const t = await page.evaluate(() => document.querySelector('#ifwrong li[data-step="replace-boot-loader"]').innerText);
    assert.match(t, /Replace the boot loader with GlassineOS’s|Replace the boot loader with GlassineOS's/);
    assert.match(t, /emergency cable/);
    assert.match(t, /Point of no return/i);
  });

  const forget = () => page.evaluate(async () => (await import('/web/records/index.js')).forgetSupplied('noteair-stock-fastboot', 'emergency-programmer'));
  const desc = fs.readFileSync(path.join(ROOT, 'devices/onyx-noteair.toml'), 'utf8');
  const descText = (k) => JSON.parse(new RegExp(`^${k} = (".*")$`, 'm').exec(desc.slice(desc.indexOf('[texts.supplied_files.emergency-programmer]')))[1]);

  await test('plan: in short, what you need and the folds, from the plan and the description', async () => {
    await forget();
    await go(page, '#/plan?plan=noteair-no-stock-developer');
    const short = await page.$eval('#short', (x) => x.innerText);
    // the description's own option text, and the backup in plain words
    assert.match(short, /Don't keep Android\. It is never started again/);
    assert.match(short, /First, a backup is made, to this computer, and checked by reading it back\./);
    assert.match(await page.$eval('#supply', (x) => x.innerText), /The Onyx Boox Note Air’s emergency programmer\s+Not chosen yet/i);
    assert.ok(await page.$eval('#choices', (d) => d.open), 'a replaced system leaves the choices folded');
    assert.match(await page.$eval('#choices > summary', (x) => x.textContent), /Change the choices · Don't keep Android · With a backup/);
    assert.ok(!(await page.$eval('#ifwrong', (d) => d.open)), 'the steps’ escapes are folded');
    assert.match(await page.$eval('#confirm', (a) => a.getAttribute('href')), /^#\/supply\?/);
    // no file needed: straight to Confirm, the choices folded
    await go(page, '#/plan?plan=pc-windows-keep');
    assert.strictEqual(await page.$('#supply'), null);
    assert.ok(!(await page.$eval('#choices', (d) => d.open)));
    assert.match(await page.$eval('#short', (x) => x.innerText), /Windows: keep it\. It still starts/);
    await go(page, '#/plan?plan=pc-two-disks-first');
    assert.match(await page.$eval('#confirm', (a) => a.getAttribute('href')), /^#\/confirm\?/);
    // backup off: no file, the danger box, the choices open
    await go(page, '#/plan?plan=noteair-no-backup');
    assert.strictEqual(await page.$('#supply'), null);
    assert.ok(await page.$('#backup-off'));
    assert.ok(await page.$eval('#choices', (d) => d.open));
    // the emergency cable is asked under What you need
    await go(page, '#/plan?plan=noteair-keep-stock');
    assert.ok(await page.$('#need input[name="hw-emergency-cable"]'));
  });

  await test('supply: the guide is the description’s, with the name to copy, and names no site', async () => {
    await forget();
    const origin = new URL(base).origin;
    await browser.defaultBrowserContext().overridePermissions(origin, ['clipboard-read', 'clipboard-write', 'clipboard-sanitized-write']);
    await go(page, '#/supply?plan=noteair-no-stock-developer');
    const t = await page.evaluate(() => document.body.innerText);
    for (const k of ['what', 'why', 'recognise']) assert.ok(t.includes(descText(k).replace(/'/g, '’')) || t.includes(descText(k)), `the description’s ${k} isn’t shown`);
    assert.match(await page.$eval('#name pre', (x) => x.textContent), /^000cc0e100000000_7be49b72f9e43372_$/);
    assert.match(await page.$eval('#matches', (x) => x.innerText), /Chip\s+000cc0e1[\s\S]*Signing key\s+7be49b72f9e4337223ccb84d6eccca4e61ce16e3602ac2008cb18b75babe6d09/);
    // SC-002: no link but the installer's own screens, no address, nothing to search with
    for (const href of await page.$$eval('a', (xs) => xs.map((x) => x.getAttribute('href')))) assert.match(href, /^#\//, `a link to ${href}`);
    assert.ok(!/https?:|www\.|\.com\b|\.org\b/.test(t), 'names a site');
    assert.strictEqual(await page.$('input[type="search"], form'), null);
    await page.bringToFront();
    await page.click('#copy');
    await page.waitForFunction(() => /^(Copied|Selected: copy it)$/.test(document.querySelector('#copy').textContent), { timeout: 5000 });
    if ((await page.$eval('#copy', (x) => x.textContent)) === 'Copied') assert.strictEqual(await page.evaluate(() => navigator.clipboard.readText()), '000cc0e100000000_7be49b72f9e43372_');
    assert.deepStrictEqual(problems, []);
  });

  await test('supply: each verdict of the core, as the core gave it', async () => {
    const the = 'the Onyx Boox Note Air';
    // known: on to Confirm at once
    await forget();
    await go(page, '#/supply?plan=noteair-no-stock-developer');
    await pickFile('known');
    let t = await page.evaluate(() => document.body.innerText);
    assert.match(t, /A known emergency programmer/);
    assert.match(t, /in development/, 'a file seen working only in development is said so');
    assert.ok(t.includes(supplied('known').sha256.match(/.{8}/g).join(' ')), 'the checksum isn’t the record’s');
    assert.match(await page.$eval('#use', (a) => a.getAttribute('href')), /^#\/confirm\?/);
    // fits-unknown: what the file names, and nothing until the box is ticked
    await pickFile('fits-unknown');
    assert.ok(await page.$eval('#use', (b) => b.tagName === 'BUTTON' && b.disabled), 'usable before the warning is confirmed');
    t = await page.$eval('#unknown', (x) => x.innerText);
    assert.match(t, /runs programs signed with test keys/);
    assert.match(t, /Chip\s+000cc0e1\s+000cc0e1\s+The same/);
    assert.match(t, new RegExp(`Signing key\\s+${supplied('fits-unknown').values.pk_hash.slice(0, 16)}…\\s+7be49b72f9e43372…\\s+Differs`));
    await page.click('#trust');
    await page.waitForFunction(() => document.querySelector('#use').tagName === 'A', { timeout: 5000 });
    // the confirmation is the file's: a new file asks again
    const input = await page.$('#file-input');
    await input.uploadFile(path.join(FILES, 'fits-unknown.bin'));
    await page.waitForFunction(() => document.querySelector('#use').tagName === 'BUTTON' && !document.querySelector('#trust').checked, { timeout: 5000 });
    // wrong chip or key: never usable
    await pickFile('wrong-chip-or-key');
    t = await page.$eval('#wrong', (x) => x.innerText);
    assert.match(t, /The device reports/i);
    assert.match(t, /Chip\s+0014d0e1\s+000cc0e1\s+0014d0e1/);
    assert.strictEqual(await page.$('#use'), null);
    // known not to work: the description's reason
    await pickFile('known-bad');
    assert.ok((await page.$eval('#known-bad', (x) => x.innerText)).includes(supplied('known-bad').reason));
    assert.match(await page.evaluate(() => document.body.innerText), new RegExp(`doesn’t work on ${the}`));
    assert.strictEqual(await page.$('#use'), null);
    // not a programmer at all
    await pickFile('not-a-programmer');
    await unfold();
    assert.match(await page.$eval('#not-programmer', (x) => x.innerText), /Not an emergency programmer[\s\S]*Not an ELF file\./);
    assert.strictEqual(await page.$('#use'), null);
    // a file refused gates Confirm, and Plan says it doesn't fit
    await go(page, '#/confirm?plan=noteair-no-stock-developer');
    assert.match(await page.$eval('#supplied', (x) => x.innerText), /never sent/);
    assert.ok(await page.$eval('#install', (b) => b.tagName === 'BUTTON' && b.disabled));
    await go(page, '#/plan?plan=noteair-no-stock-developer');
    assert.match(await page.$eval('#supply', (x) => x.innerText), /Chosen, doesn’t fit/i);
    // with no file at all, Confirm can't go on either
    await forget();
    await go(page, '#/confirm?plan=noteair-no-stock-developer');
    assert.match(await page.$eval('#supplied', (x) => x.innerText), /Not chosen yet/);
    assert.ok(await page.$eval('#install', (b) => b.tagName === 'BUTTON' && b.disabled));
    assert.deepStrictEqual(problems, []);
  });

  await test('supply: without the file, two ways on', async () => {
    await go(page, '#/supply?plan=noteair-no-stock-developer&state=nofile');
    const t = await page.$eval('#nofile', (x) => x.innerText);
    assert.ok(t.includes(plans['noteair-no-stock-developer'].backup.consequence));
    assert.match(await page.$eval('#no-backup', (a) => a.getAttribute('href')), /^#\/plan\?.*choice\.backup=off/);
    assert.match(await page.$eval('#found', (a) => a.getAttribute('href')), /^#\/supply\?/);
  });

  await test('backup refused: the programmer didn’t work, and how the device gets out', async () => {
    await go(page, '#/backup?plan=noteair-no-stock-developer&state=refused');
    const t = await page.$eval('#refused', (x) => x.innerText);
    assert.match(t, /didn’t accept the programmer, or the programmer didn’t answer the installer\. Nothing was read/);
    assert.match(t, /Hold the power button for about 10 seconds/, 'the way out isn’t the description’s');
    assert.match(await page.$eval('#another-file', (a) => a.getAttribute('href')), /^#\/supply\?/);
    // only where the backup needs a supplied file
    await go(page, '#/backup?plan=pc-windows-keep&state=refused');
    assert.strictEqual(await page.$('#refused'), null);
  });

  await page.close();
  await browser.close();
  server.close();
  process.stdout.write(`${passes} passed, ${failures} failed\n`);
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(2); });
