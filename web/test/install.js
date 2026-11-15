// SPDX-License-Identifier: GPL-2.0-or-later
// The installation in the browser (screens/run.js, lib/install.js, the core's install_run),
// against the simulated Note Air of fake-install.js, in headless Chromium, on the site as it is
// published: node web/test/install.js SITE (make web-check).
//
// The whole job, as a person goes: Start, the device's check, its restart, the release chosen on
// Confirm, a backup made earlier, Start the installation, then each mode the device restarts into
// (Android by hand with adb, the boot loaders, the RAM environment, which needs the person's
// click), a reload of the page while it waits for the RAM environment, and the resume from the
// record in the backup folder, to the end: every step done, the device restarting into
// GlassineOS, and its disk as the core's own test of the job leaves it. And a write that doesn't
// read back: the run stops, and the screen names the way out.
'use strict';

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const puppeteer = require('puppeteer-core');
const { serve } = require('./serve.js');
const { World, Tree, bind, backupFiles, extent, image, SECTOR } = require('./fake-install.js');

const ROOT = path.resolve(__dirname, '..', '..');
const SITE = path.resolve(process.argv[2] || path.join(ROOT, 'target/web-site'));
const json = (rel) => JSON.parse(fs.readFileSync(path.join(ROOT, rel), 'utf8'));
// The stock boot loader's answers: a facts snapshot of the core's tests.
function vars() {
  const v = json('core/tests/fixtures/facts/noteair-stock-fastboot.json').fastboot.variables;
  return { ...Object.fromEntries(Object.entries(v).filter(([, x]) => x !== null)), serialno: 'SIM-0000' };
}
const PLAN_Q = 'machine=live-1&choice.keep-stock=no&hardware.emergency-cable=yes&developer=1';
const BACKUP = 'Backups/onyx-noteair-backup-2026-11-15-120000';
// The programmer file of the core's tests (a synthetic one): Confirm needs it for the backup
// step even when the backup was made earlier.
const PROGRAMMER = Buffer.from(fs.readFileSync(path.join(ROOT, 'web/fixtures/supplied/fits-unknown.hex'), 'utf8').replace(/\s/g, ''), 'hex');

async function main() {
  if (!fs.existsSync(path.join(SITE, 'pkg/glassine_installer_web_bg.wasm'))) throw new Error(`${SITE}: no site with the core (make web-site)`);
  const server = await serve(0, { root: SITE });
  const base = `http://127.0.0.1:${server.address().port}/`;
  const exe = process.env.CHROMIUM || '/usr/bin/chromium';
  const noSandbox = process.env.WEB_NO_SANDBOX === '1' || process.getuid?.() === 0;
  const browser = await puppeteer.launch({ executablePath: exe, headless: true, args: [...(noSandbox ? ['--no-sandbox'] : []), '--disable-gpu'] });
  let failures = 0, passes = 0;
  const test = async (name, fn) => {
    try { await fn(); passes++; process.stdout.write(`ok - ${name}\n`); } catch (e) { failures++; process.stdout.write(`not ok - ${name}\n  ${String((e && e.stack) || e).split('\n').slice(0, 40).join('\n  ')}\n`); }
  };

  async function open(world, tree) {
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 1000 });
    const problems = [];
    page.on('console', (m) => { if (m.type() === 'error') problems.push(`console ${m.type()}: ${m.text()}`); });
    page.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
    // Leaving while it runs: the browser asks (beforeunload); the test says leave.
    const asked = [];
    page.on('dialog', (d) => { asked.push(d.type()); d.accept(); });
    await bind(page, world, tree);
    return { page, problems, asked };
  }
  const rendered = (page, route, timeout = 30000) => page.waitForFunction(
    (r) => document.documentElement.dataset.ready === r && document.documentElement.dataset.hash === location.hash, { timeout }, route);
  async function go(page, hash, route) {
    await page.evaluate((h) => { location.hash = h; }, hash);
    await rendered(page, route);
  }
  const text = (page) => page.evaluate(() => document.body.innerText);
  const records = (page, fn, ...args) => page.evaluate(async (f, a) => (await import('/records/index.js'))[f](...a), fn, args);
  // On a timeout: what the page shows and the run's state, for the failure's report.
  const waitFor = async (page, sel, timeout = 60000) => {
    try { return await page.waitForSelector(sel, { timeout }); } catch (e) {
      const t = await text(page).catch(() => '');
      const v = await records(page, 'installRun').catch(() => null);
      const { view: _v, ...rest } = v || {};
      e.message += `\n--- page:\n${t.slice(0, 1500)}\n--- run: ${JSON.stringify(rest).slice(0, 1500)}`;
      throw e;
    }
  };
  const record = (tree) => JSON.parse(tree.files.get(`${BACKUP}/install-record.json`).toString());
  const stepsDone = (tree) => {
    const r = record(tree);
    return [...new Set(r.journal.filter((e) => e.outcome && e.outcome.result === 'done').map((e) => e.step))];
  };

  // without: release roles left out (the images' release folder may carry no recovery).
  function setting({ without = [] } = {}) {
    const world = new World({ vars: vars() });
    const files = {};
    for (const role of without) {
      delete world.release.files[`${role}.img`];
      const m = JSON.parse(world.release.files['release.json']);
      delete m.artifacts[role];
      world.release.files['release.json'] = Buffer.from(JSON.stringify(m));
    }
    for (const [k, v] of Object.entries(world.release.files)) files[`Release/${k}`] = v;
    Object.assign(files, backupFiles(world, BACKUP));
    return { world, tree: new Tree(files) };
  }

  // From the check to the start, as a person goes: Supply (the programmer, its warning
  // confirmed), Confirm (the release, the erase tick, the replaced system's name, Install as
  // planned), Back up (a backup made earlier), Install (that folder, Start).
  async function confirmAndStart(page, tree) {
    await go(page, `#/supply?${PLAN_Q}`, 'supply');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'glassine-install-'));
    fs.writeFileSync(path.join(dir, 'programmer.elf'), PROGRAMMER);
    await (await page.$('#file-input')).uploadFile(path.join(dir, 'programmer.elf'));
    await page.waitForFunction(() => { const f = document.querySelector('#file'); return f && f.dataset.verdict === 'fits-unknown'; }, { timeout: 15000 });
    await page.click('#trust');
    await go(page, `#/confirm?${PLAN_Q}`, 'confirm');
    assert.match(await page.$eval('#release-name', (x) => x.textContent), /Not chosen yet/);
    tree.pick = 'Release';
    await page.click('#choose-release');
    await page.waitForFunction(() => /[?&]release=chosen-\d+/.test(location.hash) && document.documentElement.dataset.ready === 'confirm' && document.documentElement.dataset.hash === location.hash, { timeout: 15000 });
    assert.strictEqual(await page.$eval('#release-name', (x) => x.textContent), 'Release');
    assert.doesNotMatch(await page.evaluate(() => location.hash), /Release/, 'the folder’s name is in the address');
    if (await page.$('#erase-ack')) await page.click('#erase-ack');
    if (await page.$('#osname')) await page.type('#osname', await page.$eval('label[for="osname"] strong', (x) => x.textContent));
    assert.ok(!(await page.$eval('#install', (x) => x.disabled)), 'Confirm can’t be given');
    await page.click('#install');
    await rendered(page, 'backup');
    await page.click('#use-earlier');
    await rendered(page, 'install');
    assert.strictEqual(await page.$('#not-confirmed'), null);
    assert.ok(await page.$eval('#start-install', (x) => x.disabled), 'it starts before the backup is chosen');
    tree.pick = BACKUP;
    await page.click('#backup-folder-choose');
    await page.waitForFunction(() => !document.querySelector('#start-install').disabled, { timeout: 15000 });
    await page.click('#start-install');
  }

  // From Start to the moment the run waits for the RAM environment: the check, the restart, the
  // release on Confirm, the earlier backup, then each mode up to the boot loader's restart into
  // the RAM environment.
  async function toRam(page, world, tree) {
    await page.goto(base + '#/start', { waitUntil: 'load' });
    await rendered(page, 'start');
    assert.ok(await page.$('#carry-on'), 'Start offers to carry on with an installation');
    await page.click('#go-device');
    await rendered(page, 'reach');
    await page.click('#choose');
    await rendered(page, 'check');
    // The check's session restarts the device: it starts Android.
    await records(page, 'restart');
    await page.waitForFunction(async () => (await import('/records/index.js')).device().state === 'restarted', { timeout: 15000 });
    await new Promise((r) => setTimeout(r, 400));
    assert.strictEqual(world.at, 'android');

    await confirmAndStart(page, tree);

    // The device isn't on the bus (Android, no USB debugging for a browser): the person says where it is.
    await waitFor(page, '#prompt[data-kind="already"][data-mode="fastboot-stock"]');
    assert.ok(stepsDone(tree).includes('back-up'), 'the backup step is done before the device is looked for');
    await page.click('#elsewhere-android');
    // The installer would use adb: the person runs it, then chooses the device.
    await waitFor(page, '#prompt[data-kind="perform"] #adb-line');
    world.toFastboot();
    await page.click('#choose-device');
    // The boot loader is replaced, the device restarts into the GlassineOS one by itself (a device
    // the page may use already), the RAM environment is written, and the device restarts into it:
    // a new USB device, which needs the person's click.
    await waitFor(page, '#prompt[data-kind="sent"][data-mode="ram"]', 120000);
    assert.deepStrictEqual(stepsDone(tree), ['back-up', 'replace-boot-loader', 'write-ram-environment']);
    assert.ok(world.glassine(), 'abl holds the release’s boot loader');
    assert.strictEqual(world.at, 'ram');
    assert.deepStrictEqual(world.sessions.map((s) => s.filter((c) => /^(flash|oem|reboot)/.test(c))),
      [['reboot'], ['flash:abl', 'reboot-bootloader'], ['flash:recovery', 'oem reboot-recovery']]);
  }

  // The disk as the core's test of the job (core/tests/execute.rs installed()) leaves it.
  function installed(world) {
    assert.ok(world.glassine());
    assert.ok(!world.erasedWholeDisk);
    assert.strictEqual(world.ram.armed, null, 'every disk read-only again');
    const t = world.table();
    for (const n of ['boot_a', 'system_a', 'vendor_a', 'boot_b', 'system_b', 'vendor_b', 'userdata', 'recovery']) assert.ok(extent(t, n), `${n} in the table`);
    const disk = world.ram.disks[0][1];
    const k = world.release.bytes.kernel;
    const [kb] = extent(t, 'boot_b');
    assert.ok(disk.read(kb, k.length).equals(k), 'the kernel in boot_b');
    const b = world.release.bytes.base;
    const [sb] = extent(t, 'system_b');
    assert.ok(disk.read(sb, b.length).equals(b), 'the base in system_b');
    const [va, n] = extent(t, 'vendor_a'), [vb, m] = extent(t, 'vendor_b');
    assert.strictEqual(n, m);
    assert.ok(disk.read(vb, n).equals(world.vendor) && disk.read(va, n).equals(image(n, 40)), 'vendor copied to slot B');
    // slot B pinned, slot A retired (the attribute byte of each entry)
    const attrs = (name) => {
      const at = Number(t.readBigUInt64LE(512 + 72)) * SECTOR, size = t.readUInt32LE(512 + 84);
      for (let i = 0; i < t.readUInt32LE(512 + 80); i++) {
        const e = t.subarray(at + i * size, at + (i + 1) * size);
        if (e.length >= 128 && e.subarray(56, 128).toString('utf16le').replace(/\0.*$/s, '') === name) return Number((e.readBigUInt64LE(48) >> 48n) & 0xffn);
      }
      return null;
    };
    assert.strictEqual(attrs('boot_b'), 0x7f, 'slot B pinned');
    assert.strictEqual(attrs('boot_a'), 0x80, 'slot A retired');
  }

  await test('the whole job, with a reload while it waits for the RAM environment and a resume from the record', async () => {
    const { world, tree } = setting();
    const { page, problems, asked } = await open(world, tree);
    await toRam(page, world, tree);
    const before = record(tree);

    // The page is reloaded while the run waits: the browser asked first.
    await page.reload({ waitUntil: 'load' });
    assert.deepStrictEqual(asked, ['beforeunload'], 'leaving a running installation isn’t asked about');
    // The address was the install screen's: without the device's answers it offers the record.
    await rendered(page, 'install');
    assert.ok(await page.$('#open-record'), 'after a reload Install offers to carry on from the record');
    await go(page, '#/resume', 'resume');
    tree.pick = BACKUP;
    await page.click('#open-record');
    await waitFor(page, '#run-release-choose');
    let t = await text(page);
    assert.match(t, /Resume from step 4/);
    assert.deepStrictEqual(await page.$$eval('#run-steps li', (xs) => xs.map((x) => x.dataset.record)),
      ['done', 'done', 'done', ...before.journal.some((e) => e.step === 'partition-table') ? ['open'] : ['not-started'], 'not-started', 'not-started', 'not-started', 'not-started', 'not-started']);
    assert.ok(await page.$eval('#resume-install', (x) => x.disabled), 'it resumes without the release');
    tree.pick = 'Release';
    await page.click('#run-release-choose');
    await page.waitForFunction(() => !document.querySelector('#resume-install').disabled, { timeout: 15000 });
    await page.click('#resume-install');

    // Where the device is isn't known after a reload: it is looked for; the RAM environment
    // needs the person's click (the page may not use it yet).
    await waitFor(page, '#prompt[data-kind="already"][data-mode="ram"]');
    await page.click('#choose-device');
    await waitFor(page, '#end', 180000);
    t = await text(page);
    assert.match(t, /Waiting for GlassineOS to start/);
    assert.match(t, /sets itself up and restarts once/);
    assert.strictEqual(await page.$eval('#end', (x) => x.dataset.outcome), 'waiting');
    installed(world);
    await new Promise((r) => setTimeout(r, 400));
    assert.strictEqual(world.at, 'glassine', 'the device restarted into GlassineOS');
    const r = record(tree);
    assert.ok(r.final_restart && r.final_restart.sent, 'the record holds the final restart');
    assert.deepStrictEqual(stepsDone(tree), r.journal.map((e) => e.step).filter((x, i, a) => a.indexOf(x) === i));
    assert.ok(!tree.files.has(`${BACKUP}/install-record.json.partial`), 'no half-saved record left');
    // The backup itself is only read.
    assert.deepStrictEqual([...tree.files.keys()].filter((k) => k.startsWith(BACKUP) && !k.includes('install-record')).sort(),
      [`${BACKUP}/emmc-user.img.gz`, `${BACKUP}/manifest.json`]);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('a write that doesn’t read back: the run stops, the screen says why and the way out, and Resume is offered', async () => {
    const { world, tree } = setting();
    world.ram.faults = { flipWrite: 0 };
    const { page, problems } = await open(world, tree);
    await toRam(page, world, tree);
    await page.click('#choose-device');
    await waitFor(page, '#stopped', 120000);
    const leaves = await page.$eval('#stopped', (x) => x.dataset.leaves);
    assert.ok(leaves, 'the stop names where it leaves the device');
    const why = await page.$eval('#why', (x) => x.textContent);
    assert.ok(why.length > 0, 'the stop says why');
    assert.ok(await page.$('#escape'), 'the stop says the way out');
    assert.ok(await page.$('#resume-install'), 'Resume is offered');
    const v = await records(page, 'installRun');
    assert.strictEqual(v.state, 'stopped');
    assert.strictEqual(v.result.outcome, 'stopped');
    assert.ok(v.view.steps.some((x) => x.state !== 'done'), 'the record doesn’t call it done');
    assert.strictEqual(world.ram.armed, null, 'the RAM environment left disarmed');
    process.stdout.write(`  # stopped at ${v.result.step}: ${why} (${leaves})\n`);
    // The write landed wrong once: Resume finds the device where it is and finishes the job.
    await page.click('#resume-install');
    await waitFor(page, '#end', 180000);
    installed(world);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('Stop at the next safe point, with a release without a recovery: the run stops after the step, says so, and offers Resume; without the developer’s answer beside the record, Resume refuses the untried steps', async () => {
    const { world, tree } = setting({ without: ['recovery'] });
    const { page, problems } = await open(world, tree);
    await page.goto(base + '#/start', { waitUntil: 'load' });
    await rendered(page, 'start');
    await page.click('#go-device');
    await rendered(page, 'reach');
    await page.click('#choose');
    await rendered(page, 'check');
    await records(page, 'restart');
    await confirmAndStart(page, tree);
    await waitFor(page, '#prompt[data-kind="already"]');
    await page.click('#stop-install');
    await page.waitForFunction(() => /Stopping/.test(document.querySelector('#stop-install').textContent), { timeout: 5000 });
    await page.click('#elsewhere-android');
    await waitFor(page, '#prompt[data-kind="perform"]');
    world.toFastboot();
    await page.click('#choose-device');
    await waitFor(page, '#stopped', 60000);
    const t = await text(page);
    assert.match(t, /The installation is stopped/);
    assert.match(t, /After step 2, Replace the boot loader/);
    assert.deepStrictEqual(stepsDone(tree), ['back-up', 'replace-boot-loader']);
    assert.ok(world.glassine(), 'the step it stopped after is done');
    assert.ok(await page.$('#resume-install'), 'Resume is offered');
    assert.deepStrictEqual(JSON.parse(tree.files.get(`${BACKUP}/install-record.inputs.json`)), { developer: true });
    // The answer beside the record is gone: the untried steps aren't run.
    tree.files.delete(`${BACKUP}/install-record.inputs.json`);
    await page.reload({ waitUntil: 'load' });
    await rendered(page, 'install');
    tree.pick = BACKUP;
    await page.click('#open-record');
    await waitFor(page, '#run-release-choose');
    tree.pick = 'Release';
    await page.click('#run-release-choose');
    await page.waitForFunction(() => !document.querySelector('#resume-install').disabled, { timeout: 15000 });
    await page.click('#resume-install');
    await waitFor(page, '#stopped #why');
    assert.match(await page.$eval('#why', (x) => x.textContent), /untried write step/);
    assert.deepStrictEqual(stepsDone(tree), ['back-up', 'replace-boot-loader'], 'nothing more was done');
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('a backup folder that holds another installation’s record isn’t started over', async () => {
    const { world, tree } = setting();
    tree.files.set(`${BACKUP}/install-record.json`, Buffer.from('{}'));
    const { page, problems } = await open(world, tree);
    await page.goto(base + '#/start', { waitUntil: 'load' });
    await rendered(page, 'start');
    await page.click('#go-device');
    await rendered(page, 'reach');
    await page.click('#choose');
    await rendered(page, 'check');
    await records(page, 'restart');
    // Straight to Install, without Confirm: it can't start, and says where to go.
    await go(page, `#/install?${PLAN_Q}`, 'install');
    assert.ok(await page.$('#not-confirmed #to-confirm'), 'Install without Confirm doesn’t send the person back');
    assert.ok(await page.$eval('#start-install', (x) => x.disabled));
    await assert.rejects(records(page, 'startInstall', 'live-1', { choices: { 'keep-stock': 'no' }, hardware: { 'emergency-cable': true }, developer: true }), /Confirm the plan first/);
    tree.pick = BACKUP;
    await page.click('#backup-folder-choose');
    await page.waitForFunction(() => /record of another installation/.test(document.querySelector('#backup-folder').textContent), { timeout: 15000 });
    assert.ok(await page.$eval('#start-install', (x) => x.disabled));
    await new Promise((r) => setTimeout(r, 500));
    assert.strictEqual(tree.files.get(`${BACKUP}/install-record.json`).toString(), '{}', 'the record is left as it is');
    assert.deepStrictEqual(world.sessions.map((s) => s.filter((c) => !c.startsWith('getvar') && !/^x+$/.test(c))), [['reboot']], 'nothing but the check’s restart was sent');
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await browser.close();
  server.close();
  process.stdout.write(`${passes} passed, ${failures} failed\n`);
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(2); });
