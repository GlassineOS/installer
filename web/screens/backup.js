// SPDX-License-Identifier: GPL-2.0-or-later
// Back up: what the plan backs up, saved and then read back, part by part.
// With fixtures, the address picks the moment: state: saving | verifying | verified | failed |
// refused; at: the item being saved or read (0-based); pct; these show the plan's items then.
// Live, where the backup reads the device through its emergency mode with the file the person
// supplied (spec 004 FR-010), the run decides (LiveBackup): the folder it goes to, the device's
// chooser, then each image the core stores, saved, read back, kept; the address's state is
// ignored, so no address shows a backup that doesn't exist or marks a file refused. refused: the device didn't accept the file the person supplied, or it didn't answer
// (spec 004 FR-006): nothing was read. It applies only where the backup needs such a file, and
// the file picked on Supply is then marked refused by the device (records.refuseSupplied).

import { useEffect, useState } from 'preact/hooks';
import { html, Admonition, Actions, Page, Tick } from '../lib/parts.js';
import { backupView, suppliedFor, suppliedState, escapeWords } from '../lib/model.js';
import { BOOT_SETTING, BROWSERS, human, cap } from '../lib/words.js';

// One item per disk image, then the boot settings kept outside the disks, then the values unique
// to the machine (captured whether or not the backup is on, FR-010).
export function backupItems(s) {
  const p = s.plan, b = backupView(s);
  const items = b.images.map((i) => ({ name: `${i.title}, whole`, size: i.size, tech: i.tech }));
  if (b.settings.length) items.push({ name: 'How it starts', size: '', tech: b.settings.map((x) => `${x.id} (${BOOT_SETTING[x.kind] || x.kind})`).join(', ') });
  if (p.keeps.unit_values.length) items.push({ name: `Values unique to ${s.m.the}`, size: '', tech: p.keeps.unit_values.join(', ') });
  return items;
}

// The page's backup run, re-rendered when it changes; null with fixtures or none started.
function useBackupRun(s) {
  const [run, setRun] = useState(() => (s.backupRun ? s.backupRun(s.machine) : null));
  useEffect(() => (s.onBackupRun ? s.onBackupRun((r) => setRun(r && s.backupRun(s.machine))) : undefined), [s]);
  return run;
}

export function Backup({ s, state }) {
  const p = s.plan;
  const run = useBackupRun(s);
  if (p && s.liveDevice && suppliedFor(s) && backupView(s).on) return html`<${LiveBackup} s=${s} run=${run} />`;
  if (!p) return html`<${Page} s=${s} step="backup" eyebrow="Back up" title="No plan"><p><a class="il-link" href=${s.link('plan')}>Back to the plan</a></p></${Page}>`;
  const need = suppliedFor(s);
  const st = ['saving', 'verifying', 'verified', 'failed'].includes(state) || (state === 'refused' && need) ? state : 'saving';
  // The file that was refused is the one picked on Supply: it is never offered as usable again
  // in this page (004 FR-006). Marking it again is harmless.
  if (st === 'refused') {
    const { got, ok } = suppliedState(s, need);
    if (got && ok) s.refuseSupplied(s.machine, need.id, got.check.sha256);
  }
  const b = backupView(s);
  const items = backupItems(s), n = items.length;
  const at = st === 'verified' ? n : Math.min(s.num('at', Math.floor(n / 2)), n - 1);
  const pct = s.num('pct', 40);
  const reading = st !== 'saving';
  const pc = s.m.kind === 'pc';
  const unchanged = pc ? 'Nothing on the PC has changed; it starts as before.' : 'Nothing on the device has changed; it starts its system as before.';
  const total = b.on ? b.total : 'No disk image';
  const rows = items.map((it, i) => {
    if (st === 'refused') return { ...it, status: 'Not read', cls: 'ins-yn ins-yn--no', tagCls: 'ins-tag ins-tag--soft' };
    if (st === 'failed' && i === at) return { ...it, status: 'Didn’t fit', cls: 'ins-yn ins-yn--no', tagCls: 'ins-tag ins-tag--heavy' };
    if (i < at) return { ...it, status: reading ? 'Read back · matches' : 'Saved', cls: 'ins-yn ins-yn--yes', tick: true, tagCls: 'ins-tag' };
    if (i === at) return { ...it, status: reading ? 'Reading back' : 'Saving', cls: 'ins-yn ins-yn--wait', bar: true, tagCls: 'ins-tag ins-tag--heavy' };
    return { ...it, status: st === 'failed' ? 'Not checked' : reading ? 'Saved, to read back' : 'Waiting', cls: 'ins-yn ins-yn--no', tagCls: 'ins-tag ins-tag--soft' };
  });
  const T = {
    saving: ['Back up · step 1 of the plan', b.on ? `Backing up ${s.m.the}` : `Reading what is kept outside the disks`, b.on ? `The installer copies each disk the plan writes to, whole. Nothing on ${s.m.the} is written yet.` : `The backup is off, by your choice: no disk is copied. Nothing on ${s.m.the} is written yet.`],
    verifying: ['Back up · checking the copy', 'Reading the backup back', `Each part is read back from the backup and compared with ${s.m.the}, before anything is written.`],
    verified: ['Back up · done', b.on ? 'The backup is complete' : 'Read and checked', `It reads back the same as ${s.m.the}.`],
    failed: ['Back up · stopped, nothing was written', 'The backup doesn’t fit', 'The installation didn’t start.'],
    refused: ['Back up · stopped, nothing was read', need ? `The ${need.short} didn’t work` : '', 'The installation didn’t start.'],
  }[st];
  // How the device leaves the mode the file runs in: the back-up step's own escape.
  const leave = need ? escapeWords(s, need.step.on_failure, p.steps.indexOf(need.step)) : '';
  return html`<${Page} s=${s} step="backup" eyebrow=${T[0]} title=${T[1]} lead=${T[2]}>
    ${st === 'failed' ? html`<${Admonition} kind="warning" seal="G" label="The installation didn’t start" title="There isn’t room for the backup there" id="failed">
      <p>The place is full, or its disk was removed. Without a backup that reads back, the installer writes nothing.</p>
      <p><strong>Way out:</strong> ${unchanged} Choose another place with ${total} free, such as an external disk, and back up again, or go back to the plan and install without a backup.</p>
    </${Admonition}>` : null}
    ${st === 'refused' ? html`<${Admonition} kind="warning" seal="G" label="The installation didn’t start" title=${`The file didn’t work on ${s.m.the}`} id="refused">
      <p>${cap(s.m.the)} didn’t accept the ${need.short}, or the ${need.short} didn’t answer the installer. Nothing was read, and nothing on ${s.m.the} was changed.</p>
      <p><strong>Way out:</strong> ${leave} Then choose another file and back up again, or go back to the plan and choose to install without a backup.</p>
    </${Admonition}>` : null}
    <div class="ins-card" id="backup-target">
      <span class="il-label">The backup goes to</span>
      <span class="ins-card__title">${b.target.title}</span>
      <p class="ins-small">${b.target.body}</p>
      <p class="ins-small">${cap(total)}${b.atMost && b.on ? '. How much of each disk is used is known once it is read; the image holds only that, compressed.' : ''}</p>
      ${b.on ? html`<p class="ins-small ins-soft">Keep it: with it ${s.m.the} can be repaired, or the installation undone. Keep ${s.m.the} connected and this page open.</p>`
        : html`<p class="ins-small ins-soft">${b.consequence}</p>`}
    </div>
    ${st === 'verified' ? html`<div class="ins-card ins-card--strong">
      <div class="ins-row"><span class="ins-yn ins-yn--yes" aria-hidden="true"><${Tick} /></span><span class="ins-card__title">Verified by reading it back</span></div>
      <p>Every part reads back the same as on ${s.m.the}. The installation starts now.</p>
    </div>` : null}
    <section class="ins-stack">
      <h2 class="ins-h2">${st === 'refused' ? 'The parts the backup reads' : reading ? 'Read back, part by part' : 'Saved, part by part'}</h2>
      ${!n ? html`<p class="ins-small" id="backup-nothing">Nothing to read: the backup is off, and the plan names nothing outside the disks to keep.</p>` : null}
      <ul class="ins-items">${rows.map((it) => html`<li>
        <span class=${it.cls + ' ins-yn--small'} aria-hidden="true">${it.tick ? html`<${Tick} size=${12} />` : null}</span>
        <span><span style="font-weight:600">${it.name}</span> <span class="ins-items__size">${it.size}</span></span>
        <span class=${it.tagCls}>${it.status}</span>
        ${it.bar && st !== 'failed' ? html`<div class="ins-bar" role="progressbar" aria-valuenow=${pct} aria-valuemin="0" aria-valuemax="100"><span style=${'width: ' + pct + '%'}></span></div>` : null}
      </li>`)}</ul>
      <details class="il-details"><summary>Technical names</summary>
        <ul class="ins-stack ins-small" style="font-family: var(--font-mono); font-size: 13.5px">${items.map((it) => html`<li>${it.name}: ${it.tech}</li>`)}</ul>
      </details>
    </section>
    <${Actions}
      next=${st === 'refused' ? `Nothing, until ${articleOf(need.short)} ${need.short} works on ${s.m.the}.` : st === 'failed' ? 'Nothing, until there is a backup that reads back.' : st === 'verified' ? 'The installation starts with step 2.' : 'When every part reads back, the installation starts by itself.'}
      out=${st === 'failed' || st === 'refused' ? 'Stop here. ' + unchanged : 'Stop now. ' + unchanged + ' The backup stays where it is.'}
      buttons=${st === 'refused'
        ? [{ label: 'Choose another file', href: s.link('supply'), primary: true, id: 'another-file' }, { label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]
        : st === 'failed'
        ? [{ label: 'Choose another place', href: s.link('confirm'), primary: true }, { label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]
        : st === 'verified' ? [{ label: 'Continue', href: s.link('install', { state: p.steps.some((x) => x.point_of_no_return) ? 'ponr' : 'midway' }), primary: true }]
        : [{ label: 'Stop', href: s.link('start') }]} />
  </${Page}>`;
}

// An image of the run in words: the user area is the plan's disk, the boot areas its own parts.
function imageTitle(b, name) {
  const m = /^emmc-boot(\d)$/.exec(name);
  if (m) return `Boot area ${m[1]}, whole`;
  if (name === 'emmc-user' && b.images[0]) return `${b.images[0].title}, whole`;
  return name;
}

const pctOf = (n, of) => (of ? Math.min(100, Math.floor((n * 100) / of)) : 0);

export function LiveBackup({ s, run }) {
  const p = s.plan, need = suppliedFor(s), b = backupView(s), the = s.m.the;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const { got, ok: fileOk } = suppliedState(s, need);
  const unchanged = 'Nothing on the device has changed; it starts its system as before.';
  const leave = escapeWords(s, need.step.on_failure, p.steps.indexOf(need.step));
  // A run of another file than the one picked now is over: this one hasn't started.
  const rs = run && run.sha && got && got.check.sha256 !== run.sha && run.state !== 'running' ? 'idle' : run ? run.state : null;
  const r = run && rs !== run.state ? { ...run, state: 'idle', images: [], notes: [], record: null, error: null } : run;
  const cur = r && r.images.length ? r.images[r.images.length - 1] : null;
  const st = rs === 'running' ? (cur && cur.phase === 'checking' ? 'verifying' : 'saving')
    : rs === 'done' ? 'verified'
    : ['refused', 'failed', 'nothing-sent'].includes(rs) ? rs
    : 'ready';
  const rec = r && r.record;
  const restarted = rec && rec.restart === 'done';
  const restartWords = !rec ? '' : restarted ? `${cap(the)} restarts into its own system by itself.` : leave;
  const act = (f) => async () => {
    setBusy(true);
    setError(null);
    try { await f(); } catch (e) { setError(e); }
    setBusy(false);
  };
  const chooseFolder = act(() => s.chooseBackupFolder(s.machine));
  const start = act(() => s.startBackup(s.machine, s.desc.id, need.id));
  const T = {
    ready: ['Back up · step 1 of the plan', `Backing up ${the}`, `The installer reads each disk the plan writes to, whole, through the ${need.short} you chose, and stores it in a folder on this computer. Nothing on ${the} is written.`],
    saving: ['Back up · step 1 of the plan', `Backing up ${the}`, `The installer reads each disk twice and stores it. Nothing on ${the} is written.`],
    verifying: ['Back up · checking the copy', 'Reading the backup back', `Each part is read back from the folder and compared with what was read from ${the}, before anything is written.`],
    verified: ['Back up · done', 'The backup is complete', `It reads back the same as ${the}.`],
    failed: ['Back up · stopped, nothing was written', 'The backup stopped', 'The installation didn’t start.'],
    refused: ['Back up · stopped, nothing was read', `The ${need.short} didn’t work`, 'The installation didn’t start.'],
    'nothing-sent': ['Back up · stopped, nothing was sent', `${cap(the)} wasn’t read`, 'The installation didn’t start.'],
  }[st];
  if (!s.canStoreBackup()) {
    return html`<${Page} s=${s} step="backup" eyebrow="Back up · step 1 of the plan" title=${`Backing up ${the}`}>
      <${Admonition} kind="warning" seal="G" label="This browser can’t store the backup" title="Use another browser, or the command line" id="nofolder">
        <p>The backup goes into a folder on this computer, and this browser can’t write one. These can: ${BROWSERS}, version 111 or later. The installer’s command line makes the same backup: <code>glassine-install emergency-read --programmer FILE --out FOLDER</code>.</p>
        <p><strong>Way out:</strong> ${unchanged}</p>
      </${Admonition}>
      <${Actions} next="Nothing, until the installer runs where it can store the backup." out=${'Stop here. ' + unchanged}
        buttons=${[{ label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]} />
    </${Page}>`;
  }
  const rows = (r ? r.images : []).map((i) => {
    const name = imageTitle(b, i.name), size = i.size ? human(i.size) : '';
    if (i.phase === 'kept') return { name, size, status: 'Read back · matches', cls: 'ins-yn ins-yn--yes', tick: true, tagCls: 'ins-tag', file: i.file };
    if (i.phase === 'incomplete') return { name, size, status: 'Incomplete', cls: 'ins-yn ins-yn--no', tagCls: 'ins-tag ins-tag--heavy', file: i.file };
    if (i.phase === 'checking') return { name, size, status: 'Reading back', cls: 'ins-yn ins-yn--wait', bar: pctOf(i.checked, i.size), tagCls: 'ins-tag ins-tag--heavy' };
    return { name, size, status: 'Reading, twice', cls: 'ins-yn ins-yn--wait', bar: pctOf(i.done, i.size), tagCls: 'ins-tag ins-tag--heavy' };
  });
  const running = rs === 'running';
  const files = r ? r.images.filter((i) => i.file).map((i) => i.file) : [];
  return html`<${Page} s=${s} step="backup" eyebrow=${T[0]} title=${T[1]} lead=${T[2]}>
    ${st === 'refused' ? html`<${Admonition} kind="warning" seal="G" label="The installation didn’t start" title=${`The file didn’t work on ${the}`} id="refused">
      <p>${cap(the)} didn’t accept the ${need.short}, or the ${need.short} didn’t answer the installer. Nothing was read, and nothing on ${the} was changed.</p>
      <p><strong>Way out:</strong> ${restartWords} Then choose another file and back up again, or go back to the plan and choose to install without a backup.</p>
    </${Admonition}>` : null}
    ${st === 'failed' ? html`<${Admonition} kind="warning" seal="G" label="The installation didn’t start" title="The backup stopped before it was complete" id="failed-run">
      <p>${rec.message}.</p>
      <p>Images that read back the same are kept; any other is marked incomplete (its name ends in .incomplete) and is never used as a backup.</p>
      <p><strong>Way out:</strong> ${restartWords} Nothing on ${the} was written. Back up again, or go back to the plan.</p>
    </${Admonition}>` : null}
    ${st === 'nothing-sent' ? html`<${Admonition} kind="warning" seal="G" label="Nothing was sent" title=${`${cap(the)} didn’t answer as described`} id="nothing-sent">
      <p>${rec.message}.</p>
      <p><strong>Way out:</strong> ${leave} Check that the device connected is ${the}, then back up again.</p>
    </${Admonition}>` : null}
    ${r && r.state === 'not-started' ? html`<${Admonition} kind="warning" seal="G" label="Not started" title="The backup didn’t start" id="not-started">
      <p>${r.error.kind === 'no-device' ? 'No device was chosen.' : r.error.message}</p>
      <p><strong>Way out:</strong> nothing was sent. If ${the} is in its emergency mode, ${leave.charAt(0).toLowerCase() + leave.slice(1)} Or choose it again.</p>
    </${Admonition}>` : null}
    ${error ? html`<${Admonition} kind="warning" seal="G" label="Not started" title="That didn’t work" id="backup-error">
      <p>${String(error.message || error)}</p><p><strong>Way out:</strong> nothing was sent. Try again.</p>
    </${Admonition}>` : null}
    ${!fileOk && st === 'ready' ? html`<${Admonition} kind="warning" seal="G" label="No file" title=${`The ${need.short} isn’t chosen`} id="no-file">
      <p>The backup reads ${the} through it. <a class="il-link" href=${s.link('supply')}>Choose the file</a>.</p>
    </${Admonition}>` : null}
    <div class="ins-card" id="backup-target">
      <span class="il-label">The backup goes to</span>
      <span class="ins-card__title" id="folder">${r && r.folder ? r.folder : 'No folder chosen yet'}</span>
      <p class="ins-small">${r && r.folder ? 'A new folder is made in it for this backup; nothing already in it is changed.' : 'A folder on this computer or on a disk connected to it, with room for ' + b.total + '.'}</p>
      ${running || st === 'verified' ? null : html`<div class="ins-row"><button type="button" class="il-btn" id="choose-folder" disabled=${busy} onClick=${chooseFolder}>${r && r.folder ? 'Choose another folder…' : 'Choose the folder…'}</button></div>`}
      <p class="ins-small ins-soft">Keep it: with it ${the} can be repaired, or the installation undone. Keep ${the} connected and this page open while it runs.</p>
    </div>
    ${st === 'ready' ? html`<section class="ins-stack" id="connect">
      <h2 class="ins-h2">Connect ${the} in its emergency mode</h2>
      <p>Start ${the} in its emergency download mode and connect it to this computer. Then choose it in the window your browser opens: the installer sends the ${need.short}, reads, checks what it stored, and restarts ${the}.</p>
    </section>` : null}
    ${st === 'verified' ? html`<div class="ins-card ins-card--strong" id="backup-done">
      <div class="ins-row"><span class="ins-yn ins-yn--yes" aria-hidden="true"><${Tick} /></span><span class="ins-card__title">Verified by reading it back</span></div>
      <p>Every part reads back the same as on ${the}. ${restartWords}</p>
      <p class="ins-small" style="font-family: var(--font-mono); font-size: 13.5px; overflow-wrap: anywhere">${[...files, 'manifest.json'].join(' · ')}</p>
    </div>` : null}
    ${rows.length ? html`<section class="ins-stack">
      <h2 class="ins-h2">${st === 'verifying' ? 'Read back, part by part' : 'Read and stored, part by part'}</h2>
      <ul class="ins-items" id="images">${rows.map((it) => html`<li>
        <span class=${it.cls + ' ins-yn--small'} aria-hidden="true">${it.tick ? html`<${Tick} size=${12} />` : null}</span>
        <span><span style="font-weight:600">${it.name}</span> <span class="ins-items__size">${it.size}</span></span>
        <span class=${it.tagCls}>${it.status}</span>
        ${it.bar !== undefined ? html`<div class="ins-bar" role="progressbar" aria-valuenow=${it.bar} aria-valuemin="0" aria-valuemax="100"><span style=${'width: ' + it.bar + '%'}></span></div>` : null}
      </li>`)}</ul>
    </section>` : null}
    ${r && r.notes.length ? html`<details class="il-details"><summary>What the installer saw</summary>
      <pre class="ins-small" style="white-space: pre-wrap">${r.notes.join('\n')}</pre></details>` : null}
    <${Actions}
      next=${st === 'ready' ? (r && r.folder ? `Choose ${the} in your browser’s window: the backup starts.` : 'Choose the folder, then the device.')
        : running ? 'When every part reads back, the installation can start.'
        : st === 'verified' ? 'The installation starts with step 2.'
        : st === 'refused' ? `Nothing, until ${articleOf(need.short)} ${need.short} works on ${the}.`
        : 'Nothing, until there is a backup that reads back.'}
      out=${running ? 'Keep this page open. Closing it stops the backup; what is stored so far is never taken for a backup.' : 'Stop here. ' + unchanged}
      buttons=${st === 'ready' ? [{ label: 'Choose the device and back up', onClick: start, primary: true, id: 'start-backup', disabled: busy || !r || !r.folder || !fileOk }, { label: 'Back to the file', href: s.link('supply') }, { label: 'Stop here', href: s.link('start'), push: true }]
        : running ? []
        : st === 'verified' ? [{ label: 'Continue', href: s.link('install', { state: p.steps.some((x) => x.point_of_no_return) ? 'ponr' : 'midway' }), primary: true }]
        : st === 'refused' ? [{ label: 'Choose another file', href: s.link('supply'), primary: true, id: 'another-file' }, { label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]
        : [{ label: 'Back up again', onClick: start, primary: true, id: 'again', disabled: busy || !fileOk }, { label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]} />
  </${Page}>`;
}

const articleOf = (w) => (/^[aeiou]/.test(w) ? 'an' : 'a');

export { cap };
