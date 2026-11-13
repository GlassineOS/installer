// SPDX-License-Identifier: GPL-2.0-or-later
// The site with the core in the browser, in headless Chromium, against simulated devices
// (fake-usb.js) instead of a real one: no fixtures and no repository files, only the site as
// make web-site assembles it. The person's path: Start, Connect a device, the browser's chooser,
// the core's inspect over WebUSB, the check, the plan and its answers. What the core answers is
// compared with what the core's own tests say for the same facts, and every transfer the page
// sent is checked: reads only (getvar), never reboot, nothing but GET_DESCRIPTOR on control.
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
// The pkg's exports: the three records and the descriptions' source, nothing that writes.
const EXPORTS = ['check', 'default', 'description', 'initSync', 'inspect', 'plan'];
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
  // the one length; only GET_DESCRIPTOR on control; no reboot, no other request of any kind.
  const ALLOWED_CALLS = ['requestDevice', 'open', 'controlTransferIn', 'claimInterface', 'releaseInterface', 'close'];
  function readOnly(log) {
    assert.ok(log.outs.length >= 1, 'nothing was sent');
    assert.strictEqual(log.outs[0].cmd, 'getvar:product', 'the opening');
    assert.strictEqual(log.outs[0].len, 'getvar:product'.length, 'the opening is plain');
    for (const [i, t] of log.outs.entries()) {
      assert.strictEqual(t.endpoint, 1);
      if (i === 0) continue;
      if (i === 1) { assert.ok(t.allX && t.len === LEN, 'the warm-up dummy'); continue; }
      assert.strictEqual(t.len, LEN, `transfer ${i} (${t.cmd}) is not ${LEN} bytes`);
      assert.ok(t.nulPadded, `transfer ${i} is padded with something other than NUL`);
      assert.match(t.cmd, /^getvar:[A-Za-z0-9_.-]+(:[A-Za-z0-9_.-]+)?$/, `transfer ${i} is not one read: ${t.cmd}`);
      assert.notStrictEqual(t.cmd, 'getvar:all');
      assert.ok(!/serial/.test(t.cmd), `reads ${t.cmd}`);
    }
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
