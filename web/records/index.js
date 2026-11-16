// SPDX-License-Identifier: GPL-2.0-or-later
// Where the screens get the core's records. The screens call only these functions, with the
// core's own inputs and records (report format 1, plan format 1):
//
//   inspect(machine)          -> the check report of that machine
//   plan(machine, input)      -> the dry-run plan for that input, or null when there is none
//   description(id)           -> { id, name, backend, texts } of a device description, or null
//   hostSetup()               -> what a computer needs once to reach the descriptions' USB modes
//                                (host_setup_format 1): the screens' way out when the browser
//                                couldn't open or claim a device
//   connect(kind)             -> the machine "connecting" gives at once, or null (a live device)
//   connectDevice()           -> a live device: the browser's chooser, then the core's inspect
//   device()                  -> the live device's session: { machine, state, error }, or null
//   restart()                 -> restarts the live device in its session (the person's action)
//   onDevice(fn)              -> calls fn whenever device() changes; returns the unsubscribe
//   suppliedCheck(device, file, bytes)
//                             -> the core's check of a file the person picked (supplied_check_format 1)
//   supplied(machine, file)   -> the file picked for that machine in this page, or null
//   keepSupplied(machine, file, picked) / forgetSupplied(machine, file)
//   refuseSupplied(machine, file, sha256) / deviceRefused(machine, file, sha256)
//                             -> marks a file the device refused (spec 004 FR-006), by its
//                                checksum / whether it was refused
//   canStoreBackup()          -> whether this page can run the backup and store it (live, and the
//                                browser has the folder picker: lib/store.js)
//   chooseBackupFolder(machine) / startBackup(machine, device, file)
//                             -> the backup through the emergency mode (spec 004 FR-010): the
//                                folder it goes to, then the device's chooser and the core's
//                                read, stored by lib/store.js
//   stopBackup(machine)       -> stops the running backup at its next request to the device: the
//                                image being read is marked incomplete, the device restarted
//   backupRun(machine) / onBackupRun(fn)
//                             -> that run: { state, folder, images, record, error, notes }, and
//                                fn called whenever it changes
//   chooseRelease(machine) / release(machine)
//                             -> the release folder the person picked for that device (live): its
//                                manifest goes into the plan; { name, artifacts }
//   chooseEarlierBackup(machine)
//                             -> a backup folder made before (manifest.json and the user area)
//   installFolder(machine)    -> the backup folder the installation rests on: this page's finished
//                                backup, or the earlier one chosen; { name, from }
//   confirmPlan(machine, input, text) / confirmed(machine, input)
//                             -> the person confirmed this plan on Confirm (its button), for this
//                                input and release; kept in this page's memory only
//   startInstall(machine, input)
//                             -> the record of the confirmed plan into that folder, then the run;
//                                refused unless confirmPlan was given for the same plan
//   openInstall()             -> a folder with an installation record (call from a click): the
//                                record's view, to resume it after a reload or another day
//   chooseRunRelease()        -> the release folder for that resume
//   resumeInstall()           -> runs the opened record on
//   stopInstall()             -> stops after the next step the plan names a safe stop
//   installRun() / onInstallRun(fn)
//                             -> the installation of this page: see `inst` below
//
// A picked file (spec 004 FR-003) lives only in this page's memory: its bytes, the core's check
// and whether the person confirmed the warning. It is never uploaded, put in an address or kept
// in the browser's storage; a reload forgets it. The same holds for the checksums of the files
// the device refused: forgetting the picked file doesn't forget them, a reload does.
//
// Two sources, chosen by the page's address:
//
// - Live (the default): the core itself, built for the browser (web/pkg, `make web-pkg`). The
//   person picks the device in the browser's chooser; the core inspects it over WebUSB (reads
//   only: getvar) and gives a facts snapshot, kept in this page's memory as machine `live-N`,
//   with the fastboot session still open. The one command besides getvar ever sent in that
//   session is `reboot`: when the person clicks Restart it, or, best effort, when the
//   page is closed or reloaded while the session is open (pagehide). The session is kept
//   because the Note Air's boot loader only answers a fresh start: once it is closed, the page
//   can't restart the device any more, and it stays in its USB mode until its power button is
//   held. Every check report and plan is the core's answer for those facts and
//   the person's choices. The descriptions are the copies the core carries. Nothing is fetched
//   but the core: the site works without the rest of the repository.
//   The writes of an installation are the core executor's (install_run), in its own sessions,
//   after Confirm, with the release and the backup the person chose (startInstall).
//
// - Fixtures (?fixtures=1, for the screen tests): `machine` names a facts snapshot of the
//   core's tests (core/tests/fixtures/facts/), and the records are the core's own output for
//   it: the plan golden files (core/tests/golden/*.json, written by core/tests/plan.rs), the host
//   setup record (web/fixtures/host-setup.json, written by core/tests/host_setup.rs) and the
//   check reports of web/fixtures/reports/ (written by `make web-records`, checked against the
//   program by `make descriptions`). A picked file is answered with the record of the core's
//   tests (web/fixtures/supplied/, written by core/tests/supplied.rs) whose file has the same
//   checksum. An input the core's tests never planned has no record:
//   plan() returns null and the screens say so. This mode needs the repository's files and
//   reaches no device.

import { descriptionTexts } from './description-texts.js';
import { storeSupported, pickFolder, newFolder, folderName, folderStore } from '../lib/store.js';
import { openRelease, folderState, readRecord, readInputs, saveNewRecord, installHost } from '../lib/install.js';

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
  'pc-small-disk': { machine: 'pc-small-disk', input: { developer: true, release: 'base-8gib' } },
  'pc-release-too-big': { machine: 'pc-windows', input: { developer: true, release: 'pc-too-big' } },
  'pc-secure-boot-on': { machine: 'pc-secure-boot-on', input: { developer: true } },
  'pc-secure-boot-unknown': { machine: 'pc-secure-boot-unknown', input: { developer: true } },
  'noteair-release-too-big': { machine: 'noteair-stock-fastboot', input: { hardware: { 'emergency-cable': true }, developer: true, release: 'base-8gib' } },
  'noteair-developer': { machine: 'noteair-stock-fastboot', input: { developer: true } },
  'noteair-developer-cable-yes': { machine: 'noteair-stock-fastboot', input: { hardware: { 'emergency-cable': true }, developer: true } },
};

// The check reports there are records of (one per facts snapshot of the core's tests).
export const REPORTS = [
  'noteair-stock-fastboot', 'noteair-stock-adb', 'noteair-prepared-fastboot', 'noteair-conflict-fastboot',
  'unknown-fastboot', 'pc-windows', 'pc-windows-other-model', 'pc-two-disks', 'noteair-recorded-glassine-fastboot',
  'pc-small-disk', 'pc-secure-boot-on', 'pc-secure-boot-unknown',
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

// Whether this page has the answers of that machine (live; a reload forgets them).
export const knows = (machine) => machine === THIS_PC || live.has(machine);

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
  // The release the person chose for this device: its manifest is part of the plan (002 FR-019a).
  const rel = releases.get(machine);
  if (rel) answers.release = JSON.parse(rel.manifest);
  return call(() => m.plan(got.facts, JSON.stringify(answers)));
}

export async function description(id) {
  if (!id || !/^[a-z0-9-]+$/.test(id)) return null;
  if (FIXTURES) return fixtureDescription(id);
  const src = (await wasm()).description(id);
  return src ? descriptionTexts(src, `${id}.toml`) : null;
}

// ---- files the person supplies (spec 004) ----------------------------------------------------

// The test records of web/fixtures/supplied/: one per verdict, and a wrong key where the boot ROM
// enforces it (no device report).
// What a computer needs once to reach the descriptions' USB modes: the core's record.
export async function hostSetup() {
  if (FIXTURES) return load(at('../fixtures/host-setup.json'), 'json');
  return JSON.parse((await wasm()).host_setup());
}

export const SUPPLIED_RECORDS = ['known', 'fits-unknown', 'wrong-chip-or-key', 'wrong-key-enforced', 'known-bad', 'not-a-programmer'];

const hexOf = (buf) => [...new Uint8Array(buf)].map((x) => x.toString(16).padStart(2, '0')).join('');

async function fixtureSupplied(bytes) {
  const sum = hexOf(await crypto.subtle.digest('SHA-256', bytes));
  for (const name of SUPPLIED_RECORDS) {
    const r = await load(at(`../fixtures/supplied/${name}.json`), 'json');
    if (r.sha256 === sum) return r;
  }
  throw new Error('The core’s tests have no record of a file with this checksum: with fixtures, pick one of the files of web/fixtures/supplied/ (written from its .hex).');
}

// Only reads the bytes; nothing is sent anywhere.
export async function suppliedCheck(device, file, bytes) {
  if (FIXTURES) return fixtureSupplied(bytes);
  const m = await wasm();
  return call(() => m.supplied_check(device, file, bytes, ''));
}

const picked = new Map(); // `${machine} ${file}` -> { name, size, bytes, check, trusted }
export const supplied = (machine, file) => picked.get(`${machine} ${file}`) || null;
export const keepSupplied = (machine, file, p) => { picked.set(`${machine} ${file}`, p); };
export const forgetSupplied = (machine, file) => { picked.delete(`${machine} ${file}`); };

// Files the device didn't accept, or that didn't answer (spec 004 FR-006): their checksums.
// Such a file is never offered as usable again, whatever its check said; the picked one, if it
// is that file, loses its bytes.
const refusedSums = new Map(); // `${machine} ${file}` -> Set of sha256
export const deviceRefused = (machine, file, sha256) => !!(sha256 && refusedSums.get(`${machine} ${file}`)?.has(sha256));
export const refuseSupplied = (machine, file, sha256) => {
  const k = `${machine} ${file}`;
  if (!refusedSums.has(k)) refusedSums.set(k, new Set());
  refusedSums.get(k).add(sha256);
  const p = picked.get(k);
  if (p && p.check.sha256 === sha256) { p.bytes = null; p.trusted = false; }
};

// ---- the backup through the emergency mode (spec 004 FR-010) ----------------------------------

// The run of this page (one at a time), for the machine it was started for:
//   state   idle (a folder is chosen) | running | done | refused | other-device | nothing-sent |
//           failed | stopped (by the person) |
//           not-started (nothing was sent: the chooser was closed, the device's interface busy,
//           the file not sendable; error says why)
//   folder  the name of the folder the person picked, then of the new folder in it
//   images  each image the core began: { name, size, done, checked, phase: saving | checking |
//           kept | incomplete, file }
//   record  the core's record of the run (emergency_read_format 1), once it ended
//   sha     the checksum of the file it was started with
// The images are written into a new folder inside the picked one, made when the first image
// begins. A failed or refused run can be started again with the same folder: it makes another.
let run = null;
const runWatchers = new Set();
const runChanged = () => { for (const f of runWatchers) f(backupRun(run && run.machine)); };

export const canStoreBackup = () => !FIXTURES && storeSupported();

export function backupRun(machine) {
  if (!run || run.machine !== machine) return null;
  const { parent: _p, store: st, dir: _d, ...v } = run;
  v.stopping = !!(st && st.stopped());
  return { ...v, images: run.images.map((i) => ({ ...i })), notes: run.notes.slice() };
}

export function onBackupRun(fn) {
  runWatchers.add(fn);
  return () => runWatchers.delete(fn);
}

// Asks for the folder (call from a click). Resolves with the run; a closed picker changes nothing.
export async function chooseBackupFolder(machine) {
  if (run && run.state === 'running') throw new Error('The backup is running.');
  let parent;
  try { parent = await pickFolder(); } catch (e) {
    if (e && e.name === 'AbortError') return backupRun(machine);
    throw e;
  }
  run = { machine, state: 'idle', parent, folder: parent.name, images: [], record: null, error: null, notes: [] };
  runChanged();
  return backupRun(machine);
}

export function stopBackup(machine) {
  if (run && run.machine === machine && run.state === 'running' && run.store) run.store.stop();
  runChanged();
}

// Leaving the page while a backup runs ends it without a restart: the browser asks first.
if (!FIXTURES && typeof addEventListener === 'function') {
  addEventListener('beforeunload', (ev) => {
    if (run && run.state === 'running') { ev.preventDefault(); ev.returnValue = ''; }
  });
}

function onStore(r, ev) {
  const img = r.images.find((i) => i.name === ev.name);
  if (ev.kind === 'begin') r.images.push({ name: ev.name, size: null, done: 0, checked: 0, phase: 'saving', file: null });
  else if (ev.kind === 'saved' && img) { img.done = ev.done; img.size = ev.size; }
  else if (ev.kind === 'checking' && img) { img.phase = 'checking'; img.checked = ev.done; }
  else if ((ev.kind === 'kept' || ev.kind === 'incomplete') && img) { img.phase = ev.kind; img.file = ev.file; }
  else if (ev.kind === 'note') { r.notes.push(ev.line); if (r.notes.length > 50) r.notes.shift(); }
  else return;
  runChanged();
}

// Starts the backup of `machine` (call from a click: the device's chooser needs one) with the file
// picked for `file` of description `device` on Supply. Resolves with the run when it ended.
export async function startBackup(machine, device, file, scope = 'whole-disk') {
  if (!run || run.machine !== machine || !run.parent) throw new Error('Choose the folder for the backup first.');
  if (run.state === 'running') return backupRun(machine);
  const got = supplied(machine, file);
  if (!got || !got.bytes) throw new Error('The file for the backup isn’t chosen, or it may not be sent.');
  // Only a known file, or one whose warning the person confirmed on Supply (004 FR-004); the core
  // checks it again.
  const v = got.check.verdict;
  if (!(v === 'known' || (v === 'fits-unknown' && got.trusted))) throw new Error('The file for the backup isn’t a known one, and its warning isn’t confirmed: go back to the file.');
  const m = await wasm();
  const r = run, parent = r.parent, name = folderName(device);
  Object.assign(r, { state: 'running', sha: got.check.sha256, folder: parent.name, images: [], record: null, error: null, notes: [] });
  runChanged();
  const store = folderStore(async () => {
    const dir = await newFolder(parent, name);
    r.folder = `${parent.name}/${name}`;
    r.dir = dir;
    runChanged();
    return dir;
  }, (ev) => onStore(r, ev));
  r.store = store;
  try {
    r.record = JSON.parse(await m.emergency_read(device, file, got.bytes, !!got.trusted, scope, store));
    r.state = r.record.outcome;
    // The device didn't take the file, or it didn't answer (004 FR-006): never offered again.
    if (r.state === 'refused') refuseSupplied(machine, file, got.check.sha256);
  } catch (e) {
    r.state = 'not-started';
    r.error = failure(e);
  }
  r.store = null;
  runChanged();
  return backupRun(machine);
}

// ---- the installation (spec 001: the plan carried out by the core's executor) -----------------

const releases = new Map(); // machine -> the release folder picked for it (lib/install.js openRelease)
const earlier = new Map(); // machine -> a backup folder made before, picked by the person

// The confirmations given on Confirm: machine -> { key, text }. The key is the input without
// the release's name and the release itself (a reload forgets both, and the answers they rest on).
const confirmations = new Map();
const confirmKey = (machine, input) => {
  const { release: _name, ...rest } = normalInput(input);
  const rel = releases.get(machine);
  return JSON.stringify([rest.choices, rest.hardware, rest.developer, rel ? rel.manifest : null]);
};

export function confirmPlan(machine, input, text) {
  confirmations.set(machine, { key: confirmKey(machine, input), text });
}

export function confirmed(machine, input) {
  const c = confirmations.get(machine);
  return !!c && c.key === confirmKey(machine, input) && !!releases.get(machine);
}

let releaseCount = 0;
export async function chooseRelease(machine) {
  let dir;
  try { dir = await globalThis.showDirectoryPicker({ id: 'glassine-release', mode: 'read' }); } catch (e) {
    if (e && e.name === 'AbortError') return release(machine);
    throw e;
  }
  releases.set(machine, await openRelease(dir));
  releaseCount += 1;
  return release(machine);
}

export function release(machine) {
  const r = releases.get(machine);
  // tag: the release's place in the address (`release=`): which choice, never the folder's name.
  return r ? { name: r.name, artifacts: r.artifacts, tag: `chosen-${releaseCount}` } : null;
}

async function backupFolder(dir) {
  const st = await folderState(dir);
  if (!st.manifest || !st.userArea) throw new Error(`${dir.name} isn’t a finished backup of the whole device: it needs manifest.json and emmc-user.img.gz`);
  return st;
}

export async function chooseEarlierBackup(machine) {
  let dir;
  try { dir = await pickFolder(); } catch (e) {
    if (e && e.name === 'AbortError') return installFolder(machine);
    throw e;
  }
  const st = await backupFolder(dir);
  earlier.set(machine, { dir, record: st.record });
  return installFolder(machine);
}

function folderOf(machine) {
  if (run && run.machine === machine && run.state === 'done' && run.dir) return { dir: run.dir, from: 'this-page', name: run.folder };
  const e = earlier.get(machine);
  return e ? { dir: e.dir, from: 'earlier', name: e.dir.name, record: e.record } : null;
}

export function installFolder(machine) {
  const f = folderOf(machine);
  return f ? { name: f.name, from: f.from, record: !!f.record } : null;
}

// The installation of this page (one at a time):
//   state    found (a record was opened, not run yet) | running | stopping | stopped | waiting |
//            complete | not-started (nothing was run: error says why)
//   view     the core's view of the record as last saved (record_view_format 1)
//   result   how the run ended (install_run_format 1)
//   progress { step, done, total } of the step running; prompt: what the person is asked
//   folder   the backup folder's name; release: the release folder's name
let inst = null;
const instWatchers = new Set();
let instTick = 0;
const instChanged = (now) => {
  if (!now) {
    // Progress comes often: the screens hear of it at most ten times a second.
    if (instTick) return;
    instTick = setTimeout(() => { instTick = 0; instChanged(true); }, 100);
    return;
  }
  for (const f of instWatchers) f(installRun());
};

export function installRun() {
  if (!inst) return null;
  const { dir: _d, rel: _r, host: _h, cancel: _c, ...v } = inst;
  return { ...v, notes: inst.notes.slice() };
}

export function onInstallRun(fn) {
  instWatchers.add(fn);
  return () => instWatchers.delete(fn);
}

const busy = () => inst && (inst.state === 'running' || inst.state === 'stopping');

// Leaving the page while it runs: the browser asks first. A run cut off this way is resumed
// from its record (Resume).
if (!FIXTURES && typeof addEventListener === 'function') {
  addEventListener('beforeunload', (ev) => {
    if (busy()) { ev.preventDefault(); ev.returnValue = ''; }
  });
}

function view(m, text) {
  return call(() => m.install_record_view(text));
}

async function runIt() {
  const m = await wasm();
  const r = inst;
  const desc = await description(r.view.description);
  const modes = desc ? desc.modes.map((x) => ({ id: x.id, kind: x.kind })) : [];
  r.cancel = false;
  r.host = installHost({
    dir: r.dir,
    modes,
    cancelled: () => r.cancel,
    on: (ev) => {
      if (ev.kind === 'saved') { try { r.view = view(m, ev.record); } catch { /* the next save tells */ } instChanged(true); }
      else if (ev.kind === 'progress') { r.progress = { step: ev.step, done: ev.done, total: ev.total }; instChanged(); }
      else if (ev.kind === 'note') { r.notes.push(ev.line); if (r.notes.length > 50) r.notes.shift(); instChanged(); }
      else if (ev.kind === 'prompt') { r.prompt = ev.prompt; instChanged(true); }
    },
  });
  Object.assign(r, { state: 'running', result: null, error: null, progress: null, prompt: null });
  instChanged(true);
  const record = await readRecord(r.dir);
  // Untried write steps run only for a developer who asked for them: the person's answer, kept
  // beside the record (lib/install.js); without it, no.
  const developer = !!r.developer;
  try {
    r.result = JSON.parse(await m.install_run(record, developer, r.rel.files, r.host));
    r.view = view(m, r.result.record);
    r.state = r.result.outcome;
  } catch (e) {
    r.state = 'not-started';
    r.error = failure(e);
  }
  r.prompt = null;
  r.progress = null;
  instChanged(true);
  return installRun();
}

// Makes the record of the confirmed plan in the backup folder, then runs it (call from a click).
export async function startInstall(machine, input) {
  if (busy()) return installRun();
  if (!confirmed(machine, input)) throw new Error('Confirm the plan first: the installation runs only the plan you confirmed, with this release.');
  const confirmation = confirmations.get(machine).text;
  const f = folderOf(machine);
  if (!f) throw new Error('Choose the backup the installation rests on first.');
  const rel = releases.get(machine);
  if (!rel) throw new Error('Choose the release folder first.');
  if (current && current.machine === machine && current.state === 'open') throw new Error('Restart the device first: the check’s connection still holds it.');
  const m = await wasm();
  const got = liveFacts(machine);
  const { release: _fixture, ...answers } = normalInput(input);
  const p = await plan(machine, input);
  if (!p || !p.description) throw new Error('There is no plan for this device.');
  const text = (() => { try { return m.install_new(p.description, got.facts, JSON.stringify(answers), rel.manifest, confirmation, new Date().toISOString().replace(/\.\d+Z$/, 'Z')); } catch (e) { throw failure(e); } })();
  const developer = !!normalInput(input).developer;
  await saveNewRecord(f.dir, text, { developer });
  inst = { state: 'found', dir: f.dir, folder: f.name, rel, release: rel.name, developer, view: view(m, text), result: null, error: null, progress: null, prompt: null, notes: [] };
  return runIt();
}

// Opens a folder with an installation record (call from a click).
export async function openInstall() {
  if (busy()) throw new Error('The installation is running.');
  let dir;
  try { dir = await pickFolder(); } catch (e) {
    if (e && e.name === 'AbortError') return installRun();
    throw e;
  }
  const text = await readRecord(dir);
  const inputs = text ? await readInputs(dir) : null;
  if (!text) throw new Error(`${dir.name} holds no installation record (${'install-record.json'}): choose the backup folder the installation used`);
  const m = await wasm();
  inst = { state: 'found', dir, folder: dir.name, rel: null, release: null, developer: !!(inputs && inputs.developer === true), view: view(m, text), result: null, error: null, progress: null, prompt: null, notes: [] };
  instChanged(true);
  return installRun();
}

export async function chooseRunRelease() {
  if (!inst || busy()) return installRun();
  let dir;
  try { dir = await globalThis.showDirectoryPicker({ id: 'glassine-release', mode: 'read' }); } catch (e) {
    if (e && e.name === 'AbortError') return installRun();
    throw e;
  }
  inst.rel = await openRelease(dir);
  inst.release = inst.rel.name;
  instChanged(true);
  return installRun();
}

export async function resumeInstall() {
  if (!inst || busy()) return installRun();
  if (!inst.rel) throw new Error('Choose the release folder first.');
  if (current && current.state === 'open') throw new Error('Restart the device first: the check’s connection still holds it.');
  return runIt();
}

export function stopInstall() {
  if (!busy()) return;
  inst.cancel = true;
  inst.state = 'stopping';
  instChanged(true);
}
