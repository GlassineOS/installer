// SPDX-License-Identifier: GPL-2.0-or-later
// The installer's screens: routing, the session (the records of one machine) and the theme.
//
// A screen is #/<screen>?<params>. The params name the machine and the core's plan input:
//   machine=<id>               what inspecting saw (records/index.js): live-N, a device the
//                              core inspected in this page; with fixtures, a facts snapshot
//   choice.<id>=<value>        a plan choice; hardware.<id>=yes|no; developer=1
//   plan=<golden>              shorthand for the machine and input of a plan golden file (fixtures)
//   state=, at=, pct=          which moment of a screen (the screens after Confirm have no record yet)
//   voice=, scheme=, pen=      the Interleaf theme; live=1: started in the PC's live session
// The page's own query (before the #) picks the records' source: ?fixtures=1 the core's test
// records, else the core itself, in the browser (records/index.js).

import { render } from 'preact';
import { useEffect, useState } from 'preact/hooks';
import { html, Admonition, Page } from './lib/parts.js';
import { machineOf } from './lib/model.js';
import * as records from './records/index.js';
import { Start } from './screens/start.js';
import { Reach } from './screens/reach.js';
import { Check } from './screens/check.js';
import { Plan } from './screens/plan.js';
import { Confirm } from './screens/confirm.js';
import { Backup } from './screens/backup.js';
import { Install } from './screens/install.js';
import { Verify } from './screens/verify.js';
import { Resume } from './screens/resume.js';
import { Installed } from './screens/installed.js';
import { Done } from './screens/done.js';

export const SCREENS = { start: Start, reach: Reach, check: Check, plan: Plan, confirm: Confirm, backup: Backup, install: Install, verify: Verify, resume: Resume, installed: Installed, done: Done };
const CARRIED = ['voice', 'scheme', 'pen', 'live'];

function parse() {
  const m = /^#\/([a-z]*)(?:\?(.*))?$/.exec(location.hash || '');
  return { route: (m && m[1]) || 'start', q: new URLSearchParams((m && m[2]) || ''), hash: location.hash };
}

function inputOf(q) {
  const input = { choices: {}, hardware: {}, developer: q.get('developer') === '1', release: q.get('release') };
  for (const [k, v] of q) {
    if (k.startsWith('choice.')) input.choices[k.slice(7)] = v;
    if (k.startsWith('hardware.')) input.hardware[k.slice(9)] = v === 'yes';
  }
  return input;
}

async function openSession(route, q) {
  let machine = q.get('machine'), input = inputOf(q);
  const g = q.get('plan');
  if (g) {
    if (!records.FIXTURES) throw new Error('plan= names a test record: open the page with ?fixtures=1');
    if (!records.GOLDEN_PLANS[g]) throw new Error(`no plan golden file named ${g}`);
    ({ machine } = records.GOLDEN_PLANS[g]);
    input = records.normalInput(records.GOLDEN_PLANS[g].input);
  }
  const s = { route, q, machine, input, report: null, plan: null, desc: null };
  if (machine) {
    s.report = await records.inspect(machine);
    const rec = s.report.recognition;
    if (s.report.supported) s.plan = await records.plan(machine, input);
    // The description names the machine only when it matched; a candidate's texts still say
    // how to reach the mode it needs.
    const own = (s.plan && s.plan.description) || (rec.result === 'match' ? rec.device : null);
    s.desc = own ? await records.description(own) : null;
    s.candidate = s.desc || (rec.candidates[0] ? await records.description(rec.candidates[0]) : null);
  }
  s.m = machineOf(s.report, s.desc);
  // A link to another screen of the same machine, input and theme.
  s.link = (to, extra = {}, inp = s.input, mach = s.machine) => {
    const p = new URLSearchParams();
    if (mach) p.set('machine', mach);
    for (const [k, v] of Object.entries(inp.choices || {})) p.set('choice.' + k, v);
    for (const [k, v] of Object.entries(inp.hardware || {})) p.set('hardware.' + k, v ? 'yes' : 'no');
    if (inp.developer) p.set('developer', '1');
    if (inp.release) p.set('release', inp.release);
    for (const k of CARRIED) if (q.get(k)) p.set(k, q.get(k));
    for (const [k, v] of Object.entries(extra)) if (v !== undefined && v !== null) p.set(k, String(v));
    return `#/${to}?${p}`;
  };
  s.withInput = (inp) => { location.hash = s.link(route, Object.fromEntries([...q].filter(([k]) => ['state', 'at', 'pct'].includes(k))), inp); };
  s.connect = (kind) => s.link('reach', {}, { choices: {}, hardware: {} }, records.connect(kind));
  // Live: the device is picked in the browser's chooser and inspected by the core; its machine
  // comes with the answers. With fixtures the machine is known before (s.connect).
  s.liveDevice = !records.FIXTURES;
  s.connectDevice = records.connectDevice;
  // Live: the device's session after the check, kept open for Restart it (lib/device.js).
  s.device = records.device;
  s.restart = records.restart;
  s.onDevice = records.onDevice;
  s.param = (k, d) => (q.get(k) === null ? d : q.get(k));
  s.num = (k, d) => (q.get(k) === null || !/^\d+$/.test(q.get(k)) ? d : Number(q.get(k)));
  s.live = q.get('live') === '1';
  return s;
}

function theme(q) {
  const voice = ['white', 'editorial', 'stationery'].includes(q.get('voice')) ? q.get('voice') : 'white';
  const scheme = ['paper', 'night', 'eink'].includes(q.get('scheme')) ? q.get('scheme') : 'paper';
  const pen = ['1', '2', '3'].includes(q.get('pen')) ? q.get('pen') : '1';
  document.documentElement.dataset.theme = `${voice}-${scheme}`;
  document.documentElement.dataset.pen = pen;
}

function App() {
  const [view, setView] = useState({ ...parse(), s: null, error: null });
  useEffect(() => {
    let alive = true;
    const load = () => {
      const { route, q, hash } = parse();
      theme(q);
      delete document.documentElement.dataset.ready;
      openSession(route, q).then(
        (s) => alive && hash === location.hash && setView({ route, q, hash, s, error: null }),
        (e) => { console.error(e); if (alive) setView({ route, q, hash, s: null, error: e }); },
      );
    };
    load();
    addEventListener('hashchange', load);
    return () => { alive = false; removeEventListener('hashchange', load); };
  }, []);
  useEffect(() => {
    // For the tests: which screen, of which address, has rendered.
    if (view.s || view.error) {
      document.documentElement.dataset.ready = view.error ? 'error' : view.route;
      document.documentElement.dataset.hash = view.hash;
      scrollTo(0, 0);
    }
  }, [view]);

  if (!('usb' in navigator)) return html`<${Start} s=${view.s} state="nowebusb" />`;
  if (view.error) {
    return html`<${Page} step="start" eyebrow="The installer" title="This page couldn’t be shown">
      <${Admonition} kind="warning" seal="G" label="Error" title="A record couldn’t be read"><p>${String(view.error.message || view.error)}</p><p><strong>Way out:</strong> nothing has happened. <a class="il-link" href="#/start">Start again</a>.</p></${Admonition}>
    </${Page}>`;
  }
  if (!view.s) return null;
  const Screen = SCREENS[view.route];
  if (!Screen) {
    return html`<${Page} step="start" eyebrow="The installer" title="There is no such screen">
      <p><a class="il-link" href="#/start">Start again</a></p>
    </${Page}>`;
  }
  return html`<${Screen} key=${location.hash} s=${view.s} state=${view.s.param('state', undefined)} />`;
}

render(html`<${App} />`, document.getElementById('app'));
