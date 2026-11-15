// SPDX-License-Identifier: GPL-2.0-or-later
// Install, live: the plan carried out by the core's executor in this page (records/index.js
// startInstall, resumeInstall). Every screen of a running or recorded installation is drawn from
// its record (the plan as confirmed, each step's state), not from the address: after a reload the
// device's answers are gone, the record isn't.
//
//   start     the release, the backup it rests on and the device's restart, then Start
//   open      no installation in this page: choose the backup folder that holds its record
//   found     a record was opened: where it stands, the release, then Resume
//   running   the steps, the one running with its progress, what the person is asked (a device
//             that came back as a new USB device needs their click), Stop at the next safe point
//   stopped   why, where that leaves the device and the way out (the description's escape)
//   waiting   every step done: GlassineOS starts, sets itself up and restarts once
//
// The boards of GlassineOS/ui have Install (midway, stopping, stopped by the person) and Resume;
// the release choice, the device prompts during a run, a stop by failure, opening a record and
// the end screen are drawn here in their language, without a board yet.

import { useEffect, useState } from 'preact/hooks';
import { html, Admonition, Actions, Page, Tick, Cross } from '../lib/parts.js';
import { stepViews, escapeWords, machineOf } from '../lib/model.js';
import { MODE, LEAVES, cap } from '../lib/words.js';
import { useDevice, isOpen } from '../lib/device.js';

function useInstall(s) {
  const [r, setR] = useState(() => s.installRun());
  useEffect(() => s.onInstallRun(setR), [s]);
  return r;
}

// The session the step texts need (stepViews, escapeWords): the record's plan and description.
function useRecorded(s, view) {
  const [desc, setDesc] = useState(null);
  const id = view && view.description;
  useEffect(() => {
    let alive = true;
    if (id) s.description(id).then((d) => alive && setDesc(d), () => {});
    return () => { alive = false; };
  }, [id]);
  if (!view) return null;
  return { ...s, plan: view.plan, desc, m: machineOf(null, desc) };
}

const human = (n) => (n >= 1 << 30 ? `${(n / (1 << 30)).toFixed(1)} GiB` : n >= 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MiB` : `${Math.round(n / 1024)} KiB`);
const titleOf = (v) => (v.owed ? v.kind.replace(/-/g, ' ') : v.title.replace(/\.$/, ''));

// The steps with their state in the record; `now` the step running.
function RunSteps({ rs, view, now, progress, stopped }) {
  const views = stepViews(rs);
  const state = Object.fromEntries(view.steps.map((x) => [x.step, x]));
  return html`<ol class="ins-plan" aria-label="Progress" id="run-steps">
    ${views.map((v) => {
      const st = state[v.id] || { state: 'not-started' };
      const cur = v.id === now && !stopped;
      const ds = st.state === 'done' ? 'done' : cur ? 'current' : 'todo';
      const tag = st.state === 'done' ? 'Done' : cur ? 'Now' : st.state === 'failed' ? 'Didn’t write: went on past it' : st.state === 'open' || st.state === 'interrupted' ? 'Cut off' : 'Waiting';
      const pct = cur && progress && progress.step === v.id && progress.total ? Math.floor((progress.done / progress.total) * 100) : null;
      return html`<li data-state=${ds} data-step=${v.id} data-record=${st.state}>
        <span class="ins-plan__mark" aria-hidden="true">${ds === 'done' ? html`<${Tick} size=${16} />` : st.state === 'failed' ? html`<${Cross} />` : null}</span>
        <div class="ins-plan__head">
          <span class="ins-plan__title">${titleOf(v)}</span>
          ${v.ponr ? html`<span class="ins-tag ins-tag--heavy">Point of no return</span>` : null}
          <span class=${'ins-tag ' + (cur ? 'ins-tag--heavy' : 'ins-tag--soft')}>${tag}</span>
        </div>
        ${cur && v.body ? html`<p class="ins-plan__body">${v.body}</p>` : null}
        ${pct !== null ? html`<div><div class="ins-bar" role="progressbar" aria-valuenow=${pct} aria-valuemin="0" aria-valuemax="100"><span style=${'width: ' + pct + '%'}></span></div>
          <p class="ins-small ins-soft" style="margin-top:4px" id="progress">${pct} % · ${human(progress.done)} of ${human(progress.total)}</p></div>` : null}
        ${cur ? html`<p class=${'ins-escape' + (v.cable ? ' ins-escape--cable' : '')}><b>If this step fails now:</b> ${v.escape}</p>` : null}
        ${st.state === 'failed' && st.message ? html`<p class="ins-small ins-soft">${st.message}</p>` : null}
      </li>`;
    })}
  </ol>`;
}

const ELSEWHERE = { 'android-adb': 'It shows Android', 'glassine-usb': 'It shows GlassineOS' };

// What the person is asked while the run waits for the device.
function Prompt({ rs, p }) {
  const the = rs.m.the, The = cap(the);
  const where = (MODE[p.mode.kind] || p.mode.id).replace(/^Its /, 'its ').replace(/^A /, 'a ');
  const k = p.how.kind;
  const T = {
    already: ['Connect the device', `Bring ${the} into ${where}`, `The installer looks for ${the} there. If it is somewhere else, say where it is.`],
    sent: ['Allow the device', `${The} restarted into ${where}`, 'It came back as a new USB device, which this page may not use yet. Choose it, and the installation goes on.'],
    perform: ['Your turn', `Restart ${the} into ${where}`, 'The installer does this through Android’s USB debugging, which a browser can’t use. On this computer, run this in a terminal, with USB debugging on, then choose the device:'],
    ask: ['Your turn', `Bring ${the} into ${where}`, ''],
  }[k];
  return html`<div class="ins-card ins-card--strong" id="prompt" data-kind=${k} data-mode=${p.mode.id}>
    <span class="il-label">${T[0]}</span>
    <span class="ins-card__title">${T[1]}</span>
    ${T[2] ? html`<p>${T[2]}</p>` : null}
    ${k === 'perform' && p.how.transition === 'android-to-fastboot-stock' ? html`<pre class="ins-small" style="font-family: var(--font-mono)" id="adb-line">adb reboot bootloader</pre>` : null}
    ${p.how.text ? html`<p class="ins-small">${p.how.text}</p>` : null}
    ${p.error ? html`<p class="ins-small"><strong>The browser said:</strong> ${p.error}</p>` : null}
    <div class="ins-buttons" style="margin-top:12px">
      ${p.link ? html`<button type="button" class="il-btn ins-primary" id="choose-device" onClick=${() => p.choose()}>Choose the device</button>` : null}
      ${p.elsewhere.map((m) => html`<button type="button" class="il-btn" id=${'elsewhere-' + m.id} onClick=${() => p.elsewhereIs(m.id)}>${ELSEWHERE[m.kind]}</button>`)}
      <button type="button" class="il-btn ins-push" id="prompt-stop" onClick=${() => p.stop()}>Stop waiting</button>
    </div>
    <p class="ins-small ins-soft">Your browser shows a list of devices: choose ${the}. Stop waiting stops the installation here; it can be resumed from its record.</p>
  </div>`;
}

function Notes({ notes }) {
  if (!notes || !notes.length) return null;
  return html`<details class="il-details"><summary>What the installer did last</summary>
    <ul class="ins-stack ins-small ins-soft" style="font-family: var(--font-mono); font-size: 13.5px" id="notes">${notes.slice(-12).map((n) => html`<li>${n}</li>`)}</ul>
  </details>`;
}

// A file or folder the installation needs, chosen or not.
function Pick({ id, label, name, body, button, onClick, ok, busy }) {
  return html`<div class=${ok ? 'ins-card' : 'ins-card ins-card--warn'} id=${id}>
    <span class="il-label">${label}</span>
    <span class="ins-card__title" style="overflow-wrap: anywhere">${name || 'Not chosen yet'}</span>
    ${body ? html`<p class="ins-small">${body}</p>` : null}
    ${button ? html`<div class="ins-buttons"><button type="button" class="il-btn" id=${id + '-choose'} disabled=${!!busy} onClick=${onClick}>${button}</button></div>` : null}
  </div>`;
}

function useAct() {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  const act = (f) => () => {
    setErr(null);
    setBusy(true);
    let p;
    try { p = f(); } catch (e) { p = Promise.reject(e); }
    Promise.resolve(p).then(() => setBusy(false), (e) => { setBusy(false); setErr(e); });
  };
  return { busy, err, act };
}

const Problem = ({ err }) => (err ? html`<${Admonition} kind="warning" seal="G" label="Not done" title="That didn’t work" id="act-error"><p>${String(err.message || err)}</p></${Admonition}>` : null);

// Before the run: what it needs, from the screens before it.
function StartPanel({ s }) {
  const dev = useDevice(s);
  const { busy, err, act } = useAct();
  const [, tick] = useState(0);
  const rel = s.release(s.machine), folder = s.installFolder(s.machine);
  const open = isOpen(dev);
  // Only the plan the person confirmed, with this release (Confirm's button).
  const conf = s.confirmed(s.machine, s.input);
  const ok = rel && folder && !folder.record && !open && s.plan && conf;
  const refresh = (f) => act(async () => { await f(); tick((n) => n + 1); });
  return html`<${Page} s=${s} step="install" eyebrow="Install · ready to start" title="Start the installation"
      lead=${`The installer carries out the plan you confirmed, step by step, and keeps a record of each step next to the backup. If anything cuts it off, it resumes from that record.`}>
    <div class="ins-grid">
      <${Pick} id="release" label="The release" name=${rel && rel.name} ok=${!!rel} busy=${busy}
        body=${rel ? 'Its files are checked against its manifest before each is written.' : 'The folder of the GlassineOS release to install, with its release.json.'}
        button=${rel ? 'Choose another' : 'Choose the release folder'} onClick=${refresh(() => s.chooseRelease(s.machine))} />
      <${Pick} id="backup-folder" label="The backup" name=${folder && folder.name} ok=${!!folder && !folder.record} busy=${busy}
        body=${!folder ? 'The installation rests on a backup of the whole device: the one made on Back up, or one made earlier.'
          : folder.record ? 'This folder holds the record of another installation: resume that one instead (Start, then “Carry on with an installation”).'
          : folder.from === 'this-page' ? 'Made and checked on Back up. The record goes next to it.' : 'A backup made earlier. The installer checks it before the first write.'}
        button=${folder && folder.from === 'this-page' ? null : 'Choose a backup made earlier'} onClick=${refresh(() => s.chooseEarlierBackup(s.machine))} />
    </div>
    ${!conf ? html`<${Admonition} kind="warning" seal="G" label="Not confirmed" title="Confirm the plan first" id="not-confirmed">
      <p>The installation runs only the plan you confirmed, for the release you chose there. <a class="il-link" href=${s.link('confirm')} id="to-confirm">Back to Confirm</a>.</p>
    </${Admonition}>` : null}
    ${open ? html`<${Admonition} kind="warning" seal="G" label="Still connected" title="Restart the device first" id="device-open">
      <p>The check’s connection still holds it. Restart it; the installer finds it again when the first step needs it.</p>
      <div class="ins-buttons"><button type="button" class="il-btn ins-primary" id="restart" onClick=${() => s.restart()}>Restart it</button></div>
    </${Admonition}>` : null}
    <${Problem} err=${err} />
    <p class="ins-small ins-soft">Keep ${s.m.the} connected and this page open. Each time ${s.m.the} restarts into another mode, your browser may ask you to choose it again.</p>
    <${Actions} next=${ok ? 'The first steps: the backup is checked, then the device is looked for.' : !conf ? 'Nothing, until the plan is confirmed.' : 'Nothing, until the release and the backup are chosen and the device is restarted.'}
      out="Go back, or stop here. Nothing has changed."
      buttons=${[{ label: 'Start the installation', onClick: act(() => s.startInstall(s.machine, s.input)), primary: true, id: 'start-install', disabled: !ok || busy },
        { label: 'Back to Confirm', href: s.link('confirm') }, { label: 'Stop here', href: s.link('start'), push: true }]} />
  </${Page}>`;
}

function OpenPanel({ s }) {
  const { busy, err, act } = useAct();
  return html`<${Page} s=${null} step="install" eyebrow="Install · carry on" title="Carry on with an installation"
      lead="An installation keeps its record next to its backup. Choose that folder, and the installer shows where the installation stands and resumes it.">
    <${Problem} err=${err} />
    <${Actions} next="The installer reads the record. Nothing is sent to the device yet."
      out="Close this page. Nothing has happened."
      buttons=${[{ label: 'Choose the backup folder', onClick: act(() => s.openInstall()), primary: true, id: 'open-record', disabled: busy }, { label: 'Start again', href: '#/start', push: true }]} />
  </${Page}>`;
}

const RESUME = {
  next: (n) => `Next: step ${n}.`,
  interrupted: (n) => `Step ${n} was cut off: the installer checks what it left and does it again or goes on.`,
  failed: (n) => `Step ${n} failed last time: the installer tries it again.`,
  complete: () => 'Every step is done: what is left is the restart into GlassineOS.',
};

export function LiveInstall({ s }) {
  const r = useInstall(s);
  const rs = useRecorded(s, r && r.view);
  const { busy, err, act } = useAct();
  if (!r) return s.plan && s.machine ? html`<${StartPanel} s=${s} />` : html`<${OpenPanel} s=${s} />`;
  if (!rs) return null;
  const v = r.view, views = stepViews(rs), n = views.length;
  const idx = (id) => views.findIndex((x) => x.id === id);
  const firstOpen = (v.steps.find((x) => x.state !== 'done') || {}).step;
  const now = r.progress ? r.progress.step : (v.resume && v.resume.step) || firstOpen;
  const the = rs.m.the, The = cap(the);
  const running = r.state === 'running' || r.state === 'stopping';

  if (r.state === 'found' || r.state === 'not-started') {
    const at = v.resume.step ? idx(v.resume.step) + 1 : n;
    return html`<${Page} s=${{ m: rs.m }} step="install" eyebrow="Install · a recorded installation" title=${v.resume.next === 'complete' ? 'Every step is done' : `Resume from step ${at}`}
        lead=${`The record in ${r.folder}: ${RESUME[v.resume.next](at)}`}>
      ${!v.same_description ? html`<${Admonition} kind="warning" seal="G" label="Another description" title="This installer has another description of the device"><p>The record was made with another version of it. The installer refuses to resume with a different description.</p></${Admonition}>` : null}
      ${r.state === 'not-started' ? html`<${Admonition} kind="warning" seal="G" label="Not started" title="The installation didn’t start" id="not-started"><p>${r.error ? r.error.message : ''}</p><p><strong>Way out:</strong> nothing was sent to ${the}. Choose the right release or folder and resume.</p></${Admonition}>` : null}
      <${RunSteps} rs=${rs} view=${v} now=${null} stopped />
      <${Pick} id="run-release" label="The release" name=${r.release} ok=${!!r.release} busy=${busy}
        body="The same release the installation began with: each file is checked against the checksum in the record."
        button=${r.release ? 'Choose another' : 'Choose the release folder'} onClick=${act(() => s.chooseRunRelease())} />
      <${Problem} err=${err} />
      <${Actions} next=${RESUME[v.resume.next](at)} out=${`Close this page. ${The} stays as it is now; the record stays in ${r.folder}.`}
        buttons=${[{ label: 'Resume', onClick: act(() => s.resumeInstall()), primary: true, id: 'resume-install', disabled: !r.release || busy || !v.same_description }, { label: 'Close', href: '#/start', push: true }]} />
    </${Page}>`;
  }

  if (running) {
    const i = Math.max(0, idx(now));
    return html`<${Page} s=${{ m: rs.m }} step="install"
        eyebrow=${r.state === 'stopping' ? 'Install · stopping at the next safe point' : `Install · step ${i + 1} of ${n}`}
        title=${views[i] ? titleOf(views[i]) : 'Installing'}
        lead=${views[i] && views[i].ponr ? 'This is the point of no return. After this step, the installation can only be finished or undone by the installer.' : 'The installer follows the plan you confirmed, one step after another.'}>
      ${r.prompt ? html`<${Prompt} rs=${rs} p=${r.prompt} />` : html`<${Admonition} label="While it runs"><p>Keep ${the} connected and this page open. If the connection is lost anyway, open the installer again and carry on: it resumes from the record in ${r.folder}.</p></${Admonition}>`}
      <${RunSteps} rs=${rs} view=${v} now=${now} progress=${r.progress} />
      <${Notes} notes=${r.notes} />
      <${Actions} next=${views[i + 1] ? `Step ${i + 2}: ${titleOf(views[i + 1])}.` : 'The restart into GlassineOS.'}
        out=${r.state === 'stopping' ? 'The installer stops after the next step the plan names a safe point, then offers to resume.' : 'Stop at the next safe point. The record keeps where it stopped; Resume carries on from there.'}
        buttons=${[{ label: r.state === 'stopping' ? 'Stopping…' : 'Stop at the next safe point', onClick: () => s.stopInstall(), id: 'stop-install', disabled: r.state === 'stopping' }]} />
    </${Page}>`;
  }

  if (r.state === 'stopped') {
    const res = r.result, i = idx(res.step);
    const e = res.escape;
    const words = e ? escapeWords(rs, e, i) : null;
    const byPerson = /stopped by you|asked to stop|cancel/i.test(res.why || '');
    return html`<${Page} s=${{ m: rs.m }} step="install" eyebrow="Install · stopped" title=${byPerson ? 'The installation is stopped' : 'The installation stopped'}
        lead=${i < 0 ? 'Before its first step.' : byPerson ? `After step ${i + 1}, ${titleOf(views[i])}: a step the plan names a safe point.` : `At step ${i + 1}, ${titleOf(views[i])}.`}>
      <div class="ins-card ins-card--strong" id="stopped" data-leaves=${e ? e.leaves : ''}>
        <span class="il-label">${byPerson ? 'Stopped where you asked' : 'Why it stopped'}</span>
        <span class="ins-card__title" id="why">${res.why}</span>
        ${words ? html`<p id="escape">${words}</p>` : null}
        ${e && e.text && !(e.way_out || '').startsWith('transition') ? html`<p class="ins-small">${e.text}</p>` : null}
        <p class="ins-small ins-soft">${res.may_have_changed ? `It may have written to ${the} in this step.` : `Nothing was written to ${the} in this step.`}</p>
      </div>
      <${RunSteps} rs=${rs} view=${v} now=${null} stopped />
      <${Notes} notes=${r.notes} />
      <${Problem} err=${err} />
      <${Actions} next=${`Resume carries on from the record: ${RESUME[v.resume.next](v.resume.step ? idx(v.resume.step) + 1 : n)}`}
        out=${e ? (LEAVES[e.leaves] ? words : e.leaves) : `Close this page. The record stays in ${r.folder}.`}
        buttons=${[{ label: 'Resume', onClick: act(() => s.resumeInstall()), primary: true, id: 'resume-install', disabled: busy }, { label: 'Close', href: '#/start', push: true }]} />
    </${Page}>`;
  }

  // waiting | complete: every step done.
  return html`<${Page} s=${{ m: rs.m }} step="verify" eyebrow="Install · every step done" title="Waiting for GlassineOS to start"
      lead=${`GlassineOS is written. ${The} starts it now: it sets itself up and restarts once.`}>
    <div class="ins-row"><span class="ins-tag ins-tag--dashed">Not finished yet</span><span class="ins-small ins-soft">The installation counts as done once GlassineOS’s first start has been accepted.</span></div>
    <div class="ins-card" id="end" data-outcome=${r.state}><span class="il-label">On ${the}</span>
      <p>${r.result && r.result.check ? r.result.check : 'Start the device normally.'} The first start takes longer: GlassineOS sets itself up, then restarts once by itself.</p></div>
    ${r.result && r.result.went_on_past && r.result.went_on_past.length ? html`<${Admonition} label="Left out" title="Steps the installer went on past" id="went-on-past">
      <ul class="ins-stack ins-small">${r.result.went_on_past.map((w) => html`<li>${(views[idx(w.step)] && titleOf(views[idx(w.step)])) || w.step}: ${w.why || 'it wrote nothing'}</li>`)}</ul></${Admonition}>` : null}
    <${RunSteps} rs=${rs} view=${v} now=${null} stopped />
    <${Actions} next="GlassineOS’s first start, on the device." out=${`Keep the backup in ${r.folder}: with it ${the} can be repaired, or the installation undone.`}
      buttons=${[{ label: 'Close the installer', href: '#/start', primary: true, id: 'close' }]} />
  </${Page}>`;
}
