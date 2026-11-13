// SPDX-License-Identifier: GPL-2.0-or-later
// Where the screens get the core's records. The screens call only these functions, with the
// core's own inputs and records (report format 1, plan format 1):
//
//   inspect(machine)          -> the check report of that machine
//   plan(machine, input)      -> the dry-run plan for that input, or null when there is none
//   description(id)           -> { id, name, backend, texts } of a device description, or null
//   connect(kind)             -> the machine "connecting" gives at once, or null (a live device)
//   connectDevice()           -> a live device: the browser's chooser, then the core's inspect
//   device()                  -> the live device's session: { machine, state, error }, or null
//   restart()                 -> restarts the live device in its session (the person's action)
//   onDevice(fn)              -> calls fn whenever device() changes; returns the unsubscribe
//
// Two sources, chosen by the page's address:
//
// - Live (the default): the core itself, built for the browser (web/pkg, `make web-pkg`). The
//   person picks the device in the browser's chooser; the core inspects it over WebUSB (reads
//   only: getvar) and gives a facts snapshot, kept in this page's memory as machine `live-N`,
//   with the fastboot session still open. The one command besides getvar ever sent is
//   `reboot`, in that session: when the person clicks Restart it, or, best effort, when the
//   page is closed or reloaded while the session is open (pagehide). The session is kept
//   because the Note Air's boot loader only answers a fresh start: once it is closed, the page
//   can't restart the device any more, and it stays in its USB mode until its power button is
//   held. Every check report and plan is the core's answer for those facts and
//   the person's choices. The descriptions are the copies the core carries. Nothing is fetched
//   but the core: the site works without the rest of the repository.
//
// - Fixtures (?fixtures=1, for the screen tests): `machine` names a facts snapshot of the
//   core's tests (core/tests/fixtures/facts/), and the records are the core's own output for
//   it: the plan golden files (core/tests/golden/*.json, written by core/tests/plan.rs) and the
//   check reports of web/fixtures/reports/ (written by `make web-records`, checked against the
//   program by `make descriptions`). An input the core's tests never planned has no record:
//   plan() returns null and the screens say so. This mode needs the repository's files and
//   reaches no device.

import { descriptionTexts } from './description-texts.js';

const at = (path) => new URL(path, import.meta.url);

export const FIXTURES = new URLSearchParams(location.search).get('fixtures') === '1';

// Each golden file, with the facts and the input core/tests/plan.rs plans it from.
export const GOLDEN_PLANS = {
  'noteair-keep-stock': { machine: 'noteair-stock-fastboot', input: {} },
  'noteair-no-stock-developer': { machine: 'noteair-stock-fastboot', input: { choices: { 'keep-stock': 'no' }, hardware: { 'emergency-cable': true }, developer: true } },
  'noteair-cable-yes': { machine: 'noteair-stock-fastboot', input: { hardware: { 'emergency-cable': true } } },
  'noteair-cable-no': { machine: 'noteair-stock-fastboot', input: { hardware: { 'emergency-cable': false }, developer: true } },
  'pc-windows-keep': { machine: 'pc-windows', input: {} },
  'pc-windows-replace': { machine: 'pc-windows', input: { choices: { 'systems.1': 'replace' }, developer: true } },
  'noteair-no-backup': { machine: 'noteair-stock-fastboot', input: { choices: { backup: 'off' }, hardware: { 'emergency-cable': true }, developer: true } },
  'pc-windows-replace-no-backup': { machine: 'pc-windows', input: { choices: { 'systems.1': 'replace', backup: 'off' }, developer: true } },
  'pc-two-disks-first': { machine: 'pc-two-disks', input: { developer: true } },
  'pc-two-disks-second': { machine: 'pc-two-disks', input: { choices: { 'target-disk': 'disk1' }, developer: true } },
  'pc-small-disk-no-release': { machine: 'pc-small-disk', input: { developer: true } },
  'pc-release-small-disk': { machine: 'pc-small-disk', input: { developer: true, release: 'base-8gib' } },
  'noteair-release-too-big': { machine: 'noteair-stock-fastboot', input: { hardware: { 'emergency-cable': true }, developer: true, release: 'base-8gib' } },
};

// The check reports there are records of (one per facts snapshot of the core's tests).
export const REPORTS = [
  'noteair-stock-fastboot', 'noteair-stock-adb', 'noteair-prepared-fastboot', 'noteair-conflict-fastboot',
  'unknown-fastboot', 'pc-windows', 'pc-windows-other-model', 'pc-two-disks', 'noteair-recorded-glassine-fastboot',
  'pc-small-disk',
];

const cache = new Map();
function load(url, kind) {
  const key = String(url);
  if (!cache.has(key)) {
    cache.set(key, fetch(url).then((r) => {
      if (!r.ok) throw new Error(`${url.pathname}: HTTP ${r.status}`);
      return kind === 'json' ? r.json() : r.text();
    }));
  }
  return cache.get(key);
}

// `release` names a release manifest fixture of the core's tests (core/tests/fixtures/releases/):
// a fixtures-only part of the input. Live, the page has no release manifest yet, so the least
// sizes it would state are not known yet (spec 002 FR-019a).
export function normalInput(input = {}) {
  return { choices: { ...(input.choices || {}) }, hardware: { ...(input.hardware || {}) }, developer: !!input.developer, release: input.release || null };
}

// A choice left at its default is the same input as the choice not given. Defaults are per
// machine: the backup's place, for one, is the stick on a PC and this computer for a device.
async function defaults(machine) {
  const d = {};
  for (const [name, g] of Object.entries(GOLDEN_PLANS)) {
    if (g.machine !== machine) continue;
    for (const c of (await golden(name)).choices) if (c.default) d[c.choice] = c.value;
  }
  return d;
}

function sameInput(a, b, dflt) {
  const choices = (x) => Object.entries(x.choices).filter(([k, v]) => dflt[k] !== v).sort().join('|');
  const hardware = (x) => Object.entries(x.hardware).sort().join('|');
  return choices(a) === choices(b) && hardware(a) === hardware(b) && a.developer === b.developer && a.release === b.release;
}

const golden = (name) => load(at(`../../core/tests/golden/${name}.json`), 'json');

async function fixtureInspect(machine) {
  if (!REPORTS.includes(machine)) throw new Error(`no check report of ${machine}`);
  return load(at(`../fixtures/reports/${machine}.json`), 'json');
}

async function fixturePlan(machine, input) {
  const want = normalInput(input), dflt = await defaults(machine);
  for (const [name, g] of Object.entries(GOLDEN_PLANS)) {
    if (g.machine === machine && sameInput(normalInput(g.input), want, dflt)) return golden(name);
  }
  return null;
}

async function fixtureDescription(id) {
  const r = await fetch(at(`../../devices/${id}.toml`));
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`devices/${id}.toml: HTTP ${r.status}`);
  return descriptionTexts(await r.text(), `devices/${id}.toml`);
}

// The machine "connecting" gives at once. With fixtures: the stock facts of the core's tests.
// Live: a device has no machine until the person picks it (connectDevice); this PC can't be
// read from a browser, only by the installer on the GlassineOS USB stick.
export function connect(kind) {
  if (FIXTURES) return kind === 'pc' ? 'pc-windows' : 'noteair-stock-fastboot';
  return kind === 'pc' ? THIS_PC : null;
}

// The live source: the core, loaded once, and the facts of each device it inspected.
const THIS_PC = 'this-pc';
let core = null;
function wasm() {
  if (!core) {
    core = import('../pkg/glassine_installer_web.js').then(async (m) => { await m.default(); return m; });
  }
  return core;
}
// Load it with the page, so a click on "Choose the device" opens the chooser at once.
if (!FIXTURES) wasm().catch(() => {});

const live = new Map(); // machine -> { facts, report }
let inspected = 0;

// The core's failure records ({"error": KIND, "message": TEXT}) as errors with their kind.
function failure(e) {
  let r = null;
  try { r = JSON.parse(typeof e === 'string' ? e : String(e && e.message)); } catch { /* not a record */ }
  const err = new Error(r && r.message ? r.message : String((e && e.message) || e));
  err.kind = r && r.error ? r.error : 'internal';
  return err;
}
const call = (f) => { try { return JSON.parse(f()); } catch (e) { throw failure(e); } };

// The live device whose session is open, or was: { machine, conn, state, error }. state:
//   open        the session is open: Restart it can send `reboot`
//   restarting  `reboot` was sent, no answer yet
//   restarted   the device answered OKAY (or the page sent it on pagehide)
//   failed      the restart got no OKAY (error: why); the session is gone
//   unplugged   the device went away with the session open; nothing was sent
//   closed      another device was chosen; nothing was sent
let current = null;
const watchers = new Set();
const changed = () => { for (const f of watchers) f(device()); };

export function device() {
  return current ? { machine: current.machine, state: current.state, error: current.error } : null;
}

export function onDevice(fn) {
  watchers.add(fn);
  return () => watchers.delete(fn);
}

// Ends the session without sending anything.
function drop(state) {
  if (!current || current.state !== 'open') return;
  current.conn.close();
  current.state = state;
  changed();
}

// Sends `reboot` in the open session, once: the session leaves `current` before the core is
// called, so a second click, or the page closing after it, sends nothing.
export function restart() {
  if (!current || current.state !== 'open') return Promise.resolve(device());
  const got = current;
  got.state = 'restarting';
  const sent = got.conn.reboot();
  changed();
  return sent.then(
    () => { got.state = 'restarted'; changed(); return device(); },
    (e) => {
      // Gone from the bus before its answer was read: it is restarting (the disconnect said so).
      if (got.state !== 'restarted') { got.state = 'failed'; got.error = failure(e).message; changed(); }
      return device();
    },
  );
}

const FASTBOOT = (d) => !!d && (d.configurations || []).some((c) => c.interfaces.some((i) => i.alternates.some((a) =>
  a.interfaceClass === 0xff && a.interfaceSubclass === 0x42 && a.interfaceProtocol === 0x03)));

if (!FIXTURES && typeof navigator !== 'undefined' && navigator.usb) {
  // A fastboot device unplugged: the session is gone with it (no way to restart it from here).
  // One that leaves while its restart waits for the answer is restarting.
  navigator.usb.addEventListener('disconnect', (ev) => {
    if (!FASTBOOT(ev.device) || !current) return;
    if (current.state === 'restarting') { current.state = 'restarted'; changed(); } else drop('unplugged');
  });
  // The page closing or reloading with the session open: the last chance to restart the device.
  // Best effort: the browser may tear the page down before the transfer goes out.
  addEventListener('pagehide', () => {
    if (!current || current.state !== 'open') return;
    current.state = 'restarted';
    current.conn.reboot().catch(() => {});
  });
}

// Asks the browser for the device and inspects it (call from a click: the chooser needs one).
// Resolves with its machine; rejects with an error whose `kind` is the core's (no-device when
// the chooser was closed, not-fresh, busy, permission, usb, timeout, device, ...). A session
// still open from an earlier device is closed first (its interface would stay claimed).
export async function connectDevice() {
  const m = await wasm();
  drop('closed');
  let conn;
  try { conn = await m.inspect(); } catch (e) { throw failure(e); }
  const machine = `live-${++inspected}`;
  live.set(machine, { facts: conn.facts, report: null });
  current = { machine, conn, state: 'open', error: null };
  changed();
  return machine;
}

function liveFacts(machine) {
  if (machine === THIS_PC) {
    throw new Error('A browser can’t read this PC. Start the PC from the GlassineOS USB stick: the installer opens there and reads it.');
  }
  const got = live.get(machine);
  if (!got) throw new Error('The device’s answers are gone: they live only in this page, and it was reloaded or opened anew. Connect the device again.');
  return got;
}

export async function inspect(machine) {
  if (FIXTURES) return fixtureInspect(machine);
  const got = liveFacts(machine), m = await wasm();
  if (!got.report) got.report = call(() => m.check(got.facts));
  return got.report;
}

export async function plan(machine, input) {
  if (FIXTURES) return fixturePlan(machine, input);
  const got = liveFacts(machine), m = await wasm();
  const { release: _fixture, ...answers } = normalInput(input);
  return call(() => m.plan(got.facts, JSON.stringify(answers)));
}

export async function description(id) {
  if (!id || !/^[a-z0-9-]+$/.test(id)) return null;
  if (FIXTURES) return fixtureDescription(id);
  const src = (await wasm()).description(id);
  return src ? descriptionTexts(src, `${id}.toml`) : null;
}
