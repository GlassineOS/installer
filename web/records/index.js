// SPDX-License-Identifier: GPL-2.0-or-later
// Where the screens get the core's records: the one module that changes when the core's
// WebAssembly binding arrives. The screens call only these functions, with the core's own
// inputs and records (report format 1, plan format 1):
//
//   inspect(machine)          -> the check report of that machine
//   plan(machine, input)      -> the dry-run plan for that input, or null when there is none
//   description(id)           -> { id, name, backend, texts } of a device description, or null
//
// `machine` stands for what inspecting saw. Today it names a facts snapshot of the core's tests
// (core/tests/fixtures/facts/), and the records are the core's own output for it: the plan
// golden files (core/tests/golden/*.json, written by core/tests/plan.rs) and the check reports
// of web/fixtures/reports/ (written by `make web-records`, checked against the program by
// `make descriptions`). An input the core's tests never planned has no record: plan() returns
// null and the screens say so. Nothing here reaches a device.

import { descriptionTexts } from './description-texts.js';

const at = (path) => new URL(path, import.meta.url);

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
};

// The check reports there are records of (one per facts snapshot of the core's tests).
export const REPORTS = [
  'noteair-stock-fastboot', 'noteair-stock-adb', 'noteair-prepared-fastboot', 'noteair-conflict-fastboot',
  'unknown-fastboot', 'pc-windows', 'pc-windows-other-model', 'pc-two-disks',
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

export function normalInput(input = {}) {
  return { choices: { ...(input.choices || {}) }, hardware: { ...(input.hardware || {}) }, developer: !!input.developer };
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
  return choices(a) === choices(b) && hardware(a) === hardware(b) && a.developer === b.developer;
}

const golden = (name) => load(at(`../../core/tests/golden/${name}.json`), 'json');

export async function inspect(machine) {
  if (!REPORTS.includes(machine)) throw new Error(`no check report of ${machine}`);
  return load(at(`../fixtures/reports/${machine}.json`), 'json');
}

export async function plan(machine, input) {
  const want = normalInput(input), dflt = await defaults(machine);
  for (const [name, g] of Object.entries(GOLDEN_PLANS)) {
    if (g.machine === machine && sameInput(normalInput(g.input), want, dflt)) return golden(name);
  }
  return null;
}

export async function description(id) {
  if (!id || !/^[a-z0-9-]+$/.test(id)) return null;
  const r = await fetch(at(`../../devices/${id}.toml`));
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`devices/${id}.toml: HTTP ${r.status}`);
  return descriptionTexts(await r.text(), `devices/${id}.toml`);
}

// The machine "connecting" gives, until the installer reaches one over USB (or reads the PC it
// runs on): the stock facts of the core's tests.
export function connect(kind) {
  return kind === 'pc' ? 'pc-windows' : 'noteair-stock-fastboot';
}
