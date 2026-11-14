// SPDX-License-Identifier: GPL-2.0-or-later
// The site with the core in the browser, in headless Chromium, against simulated devices
// (fake-usb.js) instead of a real one: no fixtures and no repository files, only the site as
// make web-site assembles it. The person's path: Start, Connect a device, the browser's chooser,
// the core's inspect over WebUSB, the check, the plan and its answers. What the core answers is
// compared with what the core's own tests say for the same facts, and every transfer the page
// sent is checked: reads only (getvar), nothing but GET_DESCRIPTOR on control, and the one
// other command, `reboot`, only in the session of the inspect, once, after the person's Restart
// it or the page closing (pagehide); never after the device was unplugged.
//
//   node web/test/live.js SITE_DIR
'use strict';
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const puppeteer = require('puppeteer-core');
const { serve } = require('./serve.js');
const { installFakeUsb } = require('./fake-usb.js');

const ROOT = path.resolve(__dirname, '..', '..');
const SITE = path.resolve(process.argv[2] || path.join(ROOT, 'target/web-site'));
const SERIAL = 'SECRET-UNIT-1234';
const LEN = 65535;
const json = (rel) => JSON.parse(fs.readFileSync(path.join(ROOT, rel), 'utf8'));
// A device's answers: the variables of a facts snapshot of the core's tests (a FAIL for its
// nulls and anything else), and its serial number when asked.
function answers(fixture) {
  const v = json(`core/tests/fixtures/facts/${fixture}.json`).fastboot.variables;
  const out = Object.fromEntries(Object.entries(v).filter(([, x]) => x !== null));
  out.serialno = SERIAL;
  return out;
}
const BROWSER = { platform: 'browser', connections: ['usb-web'] };
// The pkg's exports: the three records, the descriptions' source and the inspect's Connection
// (its facts, and reboot in its session), nothing that writes.
const EXPORTS = ['Connection', 'check', 'default', 'description', 'emergency_info', 'initSync', 'inspect', 'plan', 'supplied_check'];
// What the site may load: its own files. Never the repository's records or descriptions.
const FORBIDDEN = /^(core|devices|fixtures|web|test)\//;

async function main() {
  if (!fs.existsSync(path.join(SITE, 'pkg/glassine_installer_web_bg.wasm'))) throw new Error(`${SITE}: no site with the core (make web-site)`);
  const requests = [];
  const server = await serve(0, { root: SITE, log: (r) => requests.push(r) });
  const base = `http://127.0.0.1:${server.address().port}/`;
  const exe = process.env.CHROMIUM || '/usr/bin/chromium';
  const noSandbox = process.env.WEB_NO_SANDBOX === '1' || process.getuid?.() === 0;
  const browser = await puppeteer.launch({ executablePath: exe, headless: true, args: [...(noSandbox ? ['--no-sandbox'] : []), '--disable-gpu'] });
  let failures = 0, passes = 0;
  const test = async (name, fn) => {
    try { await fn(); passes++; process.stdout.write(`ok - ${name}\n`); } catch (e) { failures++; process.stdout.write(`not ok - ${name}\n  ${String((e && e.stack) || e).split('\n').slice(0, 8).join('\n  ')}\n`); }
  };

  async function open(device) {
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 1000 });
    const problems = [];
    await page.setRequestInterception(true);
    page.on('request', (r) => {
      const u = new URL(r.url());
      if (u.protocol === 'data:' || u.protocol === 'blob:' || (u.hostname === '127.0.0.1' && u.port === String(server.address().port))) r.continue();
      else { problems.push(`request to another site: ${r.url()}`); r.abort(); }
    });
    page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warn') problems.push(`console ${m.type()}: ${m.text()}`); });
    page.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
    page.on('requestfailed', (r) => problems.push(`request failed: ${r.url()} ${r.failure() && r.failure().errorText}`));
    page.on('response', (r) => { if (r.status() >= 400) problems.push(`HTTP ${r.status()}: ${r.url()}`); });
    await page.evaluateOnNewDocument(installFakeUsb, device);
    return { page, problems };
  }
  const rendered = (page, route, timeout = 30000) => page.waitForFunction(
    (r) => document.documentElement.dataset.ready === r && document.documentElement.dataset.hash === location.hash, { timeout }, route);
  async function go(page, hash, route) {
    await page.evaluate((h) => { location.hash = h; }, hash);
    await rendered(page, route);
  }
  const text = (page) => page.evaluate(() => document.body.innerText);
  const usbLog = (page) => page.evaluate(() => window.__fakeUsb);
  // The site's records module, the same instance the screens use.
  const records = (page, fn, ...args) => page.evaluate(async (f, a) => {
    const r = await import('/records/index.js');
    return r[f](...a);
  }, fn, args);

  // The ids of the action bar's first buttons (those that have one), in order.
  const buttons = (page) => page.$$eval('.ins-actions .il-btn', (xs) => xs.map((x) => x.id).filter(Boolean).slice(0, 2));
  const outs = async (page) => (await usbLog(page)).outs.length;
  const pagehide = (page) => page.evaluate(() => { window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: false })); });
  // Lets queued work run (the core's futures, the fake's timers), for a check that nothing came.
  const settle = (page, ms = 500) => page.evaluate((t) => new Promise((r) => setTimeout(r, t)), ms);

  // From Start to the check, the way a person goes: two clicks in the page (the chooser is
  // the fake's, and answers at once).
  async function connect(page) {
    await page.goto(base + '#/start', { waitUntil: 'load' });
    await rendered(page, 'start');
    await page.click('#go-device');
    await rendered(page, 'reach');
    await page.click('#choose');
  }

  // Every transfer is the opening, the warm-up's dummy or one readable getvar NUL-padded to
  // the one length, and `reboot` exactly `reboots` times (0 or 1), last; the simulated device
  // decoded each as it was meant (its length bug included); only GET_DESCRIPTOR on control;
  // no other request of any kind.
  const ALLOWED_CALLS = ['requestDevice', 'open', 'controlTransferIn', 'claimInterface', 'releaseInterface', 'close'];
  function readOnly(log, { reboots = 0 } = {}) {
    assert.ok(log.outs.length >= 1, 'nothing was sent');
    assert.strictEqual(log.outs[0].cmd, 'getvar:product', 'the opening');
    assert.strictEqual(log.outs[0].len, 'getvar:product'.length, 'the opening is plain');
    for (const [i, t] of log.outs.entries()) {
      assert.strictEqual(t.endpoint, 1);
      if (i === 0) continue;
      if (i === 1) { assert.ok(t.allX && t.len === LEN, 'the warm-up dummy'); continue; }
      assert.strictEqual(t.len, LEN, `transfer ${i} (${t.cmd}) is not ${LEN} bytes`);
      assert.ok(t.nulPadded, `transfer ${i} is padded with something other than NUL`);
      assert.strictEqual(t.seen, t.cmd, `the device read transfer ${i} as ${t.seen}, not ${t.cmd}`);
      if (t.cmd === 'reboot') { assert.strictEqual(i, log.outs.length - 1, 'something was sent after the reboot'); continue; }
      assert.match(t.cmd, /^getvar:[A-Za-z0-9_.-]+(:[A-Za-z0-9_.-]+)?$/, `transfer ${i} is not one read: ${t.cmd}`);
      assert.notStrictEqual(t.cmd, 'getvar:all');
      assert.ok(!/serial/.test(t.cmd), `reads ${t.cmd}`);
    }
    assert.strictEqual(log.outs.filter((t) => t.cmd === 'reboot').length, reboots, 'reboots sent');
    for (const c of log.calls) assert.ok(ALLOWED_CALLS.includes(c.name), `the page called ${c.name}`);
    for (const c of log.calls.filter((x) => x.name === 'controlTransferIn')) {
      const [s] = c.args;
      assert.deepStrictEqual([s.requestType, s.recipient, s.request], ['standard', 'device', 6], 'a control transfer other than GET_DESCRIPTOR');
    }
    const filters = log.calls.find((c) => c.name === 'requestDevice').args[0].filters;
    assert.deepStrictEqual(filters, [{ classCode: 0xff, subclassCode: 0x42, protocolCode: 0x03 }], 'the chooser lists only fastboot interfaces');
  }

  await test('the pkg exports the records and nothing that writes', async () => {
    const { page, problems } = await open({ vars: {} });
    await page.goto(base + '#/start', { waitUntil: 'load' });
    await rendered(page, 'start');
    const keys = await page.evaluate(async () => Object.keys(await import('/pkg/glassine_installer_web.js')).sort());
    assert.deepStrictEqual(keys, EXPORTS);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('the recorded Note Air: inspected live, the core’s check equals the recorded report', async () => {
    const { page, problems } = await open({ vars: answers('noteair-recorded-glassine-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    const hash = await page.evaluate(() => location.hash);
    assert.match(hash, /^#\/check\?machine=live-1$/);
    const report = await records(page, 'inspect', 'live-1');
    // The report the program gives for the same answers read natively (make web-records): the
    // same in everything but where the installer ran.
    const want = { ...json('web/fixtures/reports/noteair-recorded-glassine-fastboot.json'), runs_on: BROWSER };
    assert.deepStrictEqual(report, want);
    // On the GlassineOS boot loader the install state can't be told: the check says so, and
    // there is no plan (the core refuses it), as the recorded report says.
    assert.strictEqual(report.supported, false);
    const t = await text(page);
    assert.match(t, /can’t tell what is on the Onyx Boox Note Air yet/);
    assert.ok((await page.$eval('.ins-head__machine', (x) => x.textContent)).includes('Onyx Boox Note Air'));
    const refused = await records(page, 'plan', 'live-1', {});
    assert.strictEqual(refused.status, 'refused');
    assert.deepStrictEqual(refused.reasons, want.reasons.slice(1));
    await go(page, '#/plan?machine=live-1', 'plan');
    assert.match(await text(page), /isn’t supported: there is no plan/);
    const log = await usbLog(page);
    readOnly(log);
    assert.ok(log.outs.length > 100, `only ${log.outs.length} transfers: not the reads the descriptions name`);
    assert.strictEqual(log.requested, 1);
    for (const s of [JSON.stringify(report), JSON.stringify(refused), t]) assert.ok(!s.includes(SERIAL), 'the serial number got into a record');
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('a stock Note Air: Check → Plan → answers, every record the core’s, one inspect', async () => {
    const { page, problems } = await open({ vars: answers('noteair-stock-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    const report = await records(page, 'inspect', 'live-1');
    // The stock fixture is hand-made and holds fewer variables than an inspect reads: the device
    // answers FAIL to the others, and the live report has them as null. All else is the same.
    const want = json('web/fixtures/reports/noteair-stock-fastboot.json');
    const { machine, ...rest } = report, { machine: wantMachine, ...wantRest } = want;
    assert.deepStrictEqual(rest, { ...wantRest, runs_on: BROWSER });
    assert.deepStrictEqual(machine.usb, wantMachine.usb);
    for (const [k, v] of Object.entries(machine.fastboot.values)) assert.strictEqual(v, k in wantMachine.fastboot.values ? wantMachine.fastboot.values[k] : null, k);
    for (const k of Object.keys(wantMachine.fastboot.values)) assert.ok(k in machine.fastboot.values, `${k} not read`);
    assert.match(await text(page), /can run GlassineOS|^This is /m);
    // The session stays open: Restart it comes first, beside the plan (which stays the main way).
    assert.deepStrictEqual(await buttons(page), ['restart', 'plan']);
    assert.ok(!(await page.$eval('#restart', (x) => x.classList.contains('ins-primary'))));
    assert.match(await text(page), /left in its boot loader’s USB mode/);
    const sent = (await usbLog(page)).outs.length;
    await page.click('#plan');
    await rendered(page, 'plan');
    assert.ok(await page.$('#needs-answers'), 'the plan has a question first');
    const golden = (g) => json(`core/tests/golden/${g}.json`);
    assert.deepStrictEqual((await records(page, 'plan', 'live-1', {})).steps.map((x) => x.id), golden('noteair-keep-stock').steps.map((x) => x.id));
    // Answering re-plans from the same facts: no new inspect.
    await page.evaluate(() => document.querySelectorAll('input[name="hw-emergency-cable"]')[0].click());
    await page.waitForFunction(() => document.documentElement.dataset.hash.includes('hardware.emergency-cable=yes') && document.documentElement.dataset.hash === location.hash && document.documentElement.dataset.ready === 'plan', { timeout: 15000 });
    await page.waitForSelector('#developer-only', { timeout: 15000 });
    const p = await records(page, 'plan', 'live-1', { hardware: { 'emergency-cable': true } });
    assert.strictEqual(p.status, golden('noteair-cable-yes').status);
    assert.deepStrictEqual(p.steps.map((x) => x.id), golden('noteair-cable-yes').steps.map((x) => x.id));
    assert.strictEqual(await page.$$eval('li[data-step]', (x) => x.length), p.steps.length);
    // A choice the core's tests never planned still gets the core's answer (with fixtures it had none).
    await page.evaluate(() => document.querySelectorAll('input[name="hw-emergency-cable"]')[1].click());
    await page.waitForFunction(() => document.documentElement.dataset.hash.includes('hardware.emergency-cable=no') && document.documentElement.dataset.hash === location.hash && document.documentElement.dataset.ready === 'plan', { timeout: 15000 });
    await page.waitForSelector('#refused', { timeout: 15000 });
    assert.strictEqual(await page.$('#no-record'), null);
    const log = await usbLog(page);
    assert.strictEqual(log.outs.length, sent, 'the plan sent something to the device');
    assert.strictEqual(log.requested, 1);
    readOnly(log);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('Restart it after the check: one padded reboot in the same session, nothing else', async () => {
    const { page, problems } = await open({ vars: answers('noteair-recorded-glassine-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    // The device has to be reached in another mode: Restart it is the first and the main action.
    assert.deepStrictEqual(await buttons(page), ['restart', 'reach-again']);
    assert.ok(await page.$eval('#restart', (x) => x.classList.contains('ins-primary')));
    assert.ok(!(await page.$eval('#reach-again', (x) => x.classList.contains('ins-primary'))));
    const way = await page.$eval('#left-in-usb-mode', (x) => x.textContent);
    assert.match(way, /left in its boot loader’s USB mode/);
    assert.match(way, /Closing or reloading this page tries to restart it/);
    const before = await usbLog(page);
    readOnly(before);
    const n = before.outs.length, calls = before.calls.length;
    await page.click('#restart');
    await page.waitForSelector('#device-restarted', { timeout: 15000 });
    let log = await usbLog(page);
    assert.strictEqual(log.outs.length, n + 1, 'Restart it sent more or less than one transfer');
    const last = log.outs[n];
    assert.deepStrictEqual([last.cmd, last.seen, last.len, last.nulPadded], ['reboot', 'reboot', LEN, true]);
    readOnly(log, { reboots: 1 });
    // Not a new session: no new chooser, open or claim; only the interface let go afterwards.
    assert.deepStrictEqual(log.calls.slice(calls).map((c) => c.name).filter((x) => x !== 'releaseInterface'), []);
    assert.strictEqual(log.requested, 1);
    assert.strictEqual(await page.$('#restart'), null, 'Restart it is still offered');
    // The device leaves the bus; closing the page now sends nothing.
    await page.waitForFunction(() => window.__fakeUsb.gone, { timeout: 5000 });
    await pagehide(page);
    await settle(page);
    log = await usbLog(page);
    assert.strictEqual(log.outs.length, n + 1, 'something was sent after the restart');
    assert.ok(!log.calls.some((c) => c.name === 'transferOut-unplugged'));
    assert.ok(await page.$('#device-restarted'));
    // Stop here: Start says what became of it, and offers no restart.
    await page.click('#stop');
    await rendered(page, 'start');
    assert.ok(await page.$('#device-restarted'));
    assert.strictEqual(await page.$('#restart'), null);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('Stop here and Reach it again with the session open: Restart it comes first there too', async () => {
    const { page, problems } = await open({ vars: answers('noteair-recorded-glassine-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    const n = await outs(page);
    await page.click('#stop');
    await rendered(page, 'start');
    assert.ok(await page.$('#device-open #restart'), 'Start offers no restart');
    assert.match(await page.$eval('#device-open', (x) => x.textContent), /left in its USB mode.*Closing or reloading this page tries to restart it/s);
    await go(page, '#/check?machine=live-1', 'check');
    await page.click('#reach-again');
    await rendered(page, 'reach');
    assert.ok(await page.$('#device-open #restart'), 'Reach offers no restart');
    // It can't be read again before the restart: choosing would let go of its session.
    assert.ok(await page.$eval('#choose', (x) => x.disabled), 'Choose the device is offered with the session open');
    assert.strictEqual(await outs(page), n, 'leaving the check sent something');
    await page.click('#device-open #restart');
    await page.waitForSelector('#device-restarted', { timeout: 15000 });
    assert.ok(!(await page.$eval('#choose', (x) => x.disabled)), 'Choose the device stays off after the restart');
    assert.strictEqual((await usbLog(page)).requested, 1);
    const log = await usbLog(page);
    assert.strictEqual(log.outs.length, n + 1);
    readOnly(log, { reboots: 1 });
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('a boot loader that restarts without answering: the page says it is restarting', async () => {
    const { page, problems } = await open({ vars: answers('noteair-recorded-glassine-fastboot'), silentReboot: true });
    await connect(page);
    await rendered(page, 'check');
    const n = await outs(page);
    await page.click('#restart');
    await page.waitForSelector('#device-restarted', { timeout: 15000 });
    // The core's wait for the answer ends when the device leaves; it must not turn into a failure.
    await page.waitForFunction(() => window.__fakeUsb.calls.some((c) => c.name === 'releaseInterface' && window.__fakeUsb.gone), { timeout: 15000 });
    await settle(page);
    assert.strictEqual(await page.$('#device-failed'), null);
    assert.ok(await page.$('#device-restarted'));
    const log = await usbLog(page);
    assert.strictEqual(log.outs.length, n + 1);
    assert.strictEqual(log.calls.filter((c) => c.name === 'transferIn-unanswered').length, 1, 'the wait for the reboot’s answer');
    readOnly({ ...log, calls: log.calls.filter((c) => c.name !== 'transferIn-unanswered') }, { reboots: 1 });
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('unplugged after the check: the session ends, nothing is sent, the page says how to get out', async () => {
    const { page, problems } = await open({ vars: answers('noteair-recorded-glassine-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    const n = await outs(page);
    await page.evaluate(() => window.__fakeUsb.unplug());
    await page.waitForSelector('#device-unplugged', { timeout: 15000 });
    assert.match(await text(page), /Hold its power button/);
    assert.strictEqual(await page.$('#restart'), null, 'Restart it offered for a device that is gone');
    assert.strictEqual(await page.$('#left-in-usb-mode'), null);
    // Closing the page now tries nothing either.
    await pagehide(page);
    await settle(page);
    const log = await usbLog(page);
    assert.strictEqual(log.outs.length, n, 'something was sent after the unplug');
    assert.ok(!log.calls.some((c) => c.name === 'transferOut-unplugged'), 'the page tried to send to the unplugged device');
    readOnly(log);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('the page closed with the session open: pagehide sends one padded reboot, once', async () => {
    const { page, problems } = await open({ vars: answers('noteair-stock-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    const n = await outs(page);
    await pagehide(page);
    await page.waitForFunction((k) => window.__fakeUsb.outs.length > k, { timeout: 15000 }, n);
    await pagehide(page);
    await settle(page);
    const log = await usbLog(page);
    assert.strictEqual(log.outs.length, n + 1, 'pagehide sent more or less than one transfer');
    readOnly(log, { reboots: 1 });
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('the page reloaded with the session open: the reboot goes out before it is gone', async () => {
    const { page } = await open({ vars: answers('noteair-stock-fastboot') });
    await connect(page);
    await rendered(page, 'check');
    const n = await outs(page);
    await page.reload({ waitUntil: 'load' });
    // What the fake recorded as each transfer was made, kept across the reload.
    const kept = await page.evaluate(() => JSON.parse(sessionStorage.getItem('fakeUsb.outs') || '[]'));
    assert.strictEqual(kept.length, n + 1, `${kept.length - n} transfers on reload`);
    const last = kept[kept.length - 1];
    assert.deepStrictEqual([last.cmd, last.seen, last.len, last.nulPadded], ['reboot', 'reboot', LEN, true]);
    assert.strictEqual(kept.filter((t) => t.cmd === 'reboot').length, 1);
    assert.ok(kept.slice(2, -1).every((t) => /^getvar:/.test(t.cmd) && t.seen === t.cmd), 'not reads before the reboot');
    await page.close();
  });

  await test('a device not freshly started: the core refuses, the page asks for a fresh start', async () => {
    const { page, problems } = await open({ vars: answers('noteair-recorded-glassine-fastboot'), warm: true });
    await connect(page);
    await rendered(page, 'reach');
    await page.waitForFunction(() => location.hash.includes('state=notfresh') && document.documentElement.dataset.hash === location.hash, { timeout: 30000 });
    assert.match(await text(page), /Not freshly started/);
    const log = await usbLog(page);
    // The opening answered FAIL (the buffer held an old command): nothing more was sent.
    assert.strictEqual(log.outs.length, 1, 'went on after the opening was refused');
    readOnly(log);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('the chooser closed without a device: the page says so, nothing sent', async () => {
    const { page, problems } = await open({ vars: {}, cancel: true });
    await connect(page);
    await page.waitForSelector('#inspect-failed', { timeout: 30000 });
    assert.match(await text(page), /No device was chosen/);
    const log = await usbLog(page);
    assert.deepStrictEqual(log.outs, []);
    assert.deepStrictEqual(log.calls.map((c) => c.name), ['requestDevice']);
    assert.deepStrictEqual(problems, []);
    await page.close();
  });

  await test('a reload loses the device’s answers: the page says to connect again', async () => {
    const { page } = await open({ vars: {} });
    await page.goto(base + '#/check?machine=live-1', { waitUntil: 'load' });
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'error', { timeout: 30000 });
    assert.match(await text(page), /Connect the device again/);
    await page.close();
  });

  await test('the site loaded only its own files, the core among them', async () => {
    const bad = requests.filter((r) => r === null || FORBIDDEN.test(r));
    assert.deepStrictEqual(bad, []);
    assert.ok(requests.includes('pkg/glassine_installer_web_bg.wasm'), 'the core was never loaded');
  });

  await browser.close();
  server.close();
  process.stdout.write(`${passes} passed, ${failures} failed\n`);
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(2); });
