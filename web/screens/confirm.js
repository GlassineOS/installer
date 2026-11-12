// SPDX-License-Identifier: GPL-2.0-or-later
// Confirm: the whole plan once more, one confirmation (spec 001 FR-006): a tick for files that
// are erased, a replaced system confirmed by typing its name, and where the backup goes.
// state: noroom (the place chosen for the backup has no room; there is no record of it yet).
// The place is the plan's backup-to choice: changing it asks for the plan of that input.

import { useState } from 'preact/hooks';
import { html, Admonition, Actions, Page, Steps } from '../lib/parts.js';
import { list } from '../lib/words.js';
import { stepViews, erase, fate, keptSystems, goneSystems, backupView, targetOptions } from '../lib/model.js';
import { EraseNote } from './check.js';
import { NoRecord } from './plan.js';

const confirmWord = (g) => (g.kind === 'stock' ? g.label : g.display);

export function Confirm({ s, state }) {
  const p = s.plan;
  const [ack, setAck] = useState(false);
  const [typed, setTyped] = useState('');
  if (!p) {
    return html`<${Page} s=${s} step="confirm" eyebrow="Confirm" title="Confirm the plan"><${NoRecord} s=${s} what="these choices" />
      <${Actions} next="Nothing, until there is a plan." out="Stop here. Nothing was changed." buttons=${[{ label: 'Back to the plan', href: s.link('plan') }]} /></${Page}>`;
  }
  const e = erase(s), gone = goneSystems(p), kept = keptSystems(p);
  const needAck = !!e && !e.withSystem;
  const word = gone.length ? confirmWord(gone[0]) : null;
  const typedOk = !word || typed.trim().toLowerCase() === word.toLowerCase();
  const noRoom = state === 'noroom';
  const ready = p.status === 'ready';
  const ok = ready && (!needAck || ack) && typedOk && !noRoom;
  const pc = s.m.kind === 'pc';
  const b = backupView(s);
  const place = (v) => s.withInput({ ...s.input, choices: { ...s.input.choices, 'backup-to': v } });
  const next = !ready ? 'Nothing: this plan can’t be carried out as it is. Go back to the plan.'
    : needAck && !ack ? 'Tick the box under “Erases your files” to install.'
    : !typedOk ? `Type the name of the system that is replaced to install.`
    : noRoom ? 'Nothing, until there is a place with room for the backup.'
    : b.on ? `The backup starts. It is checked by reading it back before anything on ${s.m.the} is written.`
    : `The installation starts, without a backup: first the values unique to ${s.m.the} are read.`;
  return html`<${Page} s=${s} step="confirm" eyebrow="Confirm · one confirmation for the whole plan" title="Confirm the plan"
      lead=${`This is everything the installer will do on ${s.m.the}. When you confirm, it starts: ${b.on ? 'first the backup, then ' : ''}the steps below, and nothing else.`}>
    ${!ready ? html`<${Admonition} kind="warning" seal="G" label="Can’t be confirmed" title="This plan isn’t ready"><p>The plan says: ${p.status.replace(/-/g, ' ')}. <a class="il-link" href=${s.link('plan')}>Back to the plan</a>.</p></${Admonition}>` : null}
    <section class="ins-stack">
      <h2 class="ins-h2">What happens, in order</h2>
      <${Steps} views=${stepViews(s)} mode="summary" />
    </section>
    <div class="ins-grid">
      <section class="ins-card"><span class="il-label">Kept</span><ul class="ins-stack ins-small">${fate(s).keep.map((k) => html`<li>${k}</li>`)}</ul></section>
      <section class="ins-card"><span class="il-label">Systems replaced</span><p class="ins-small">${gone.length ? gone.map((g) => `${g.display}: ${g.outcome}.`).join(' ') : `None. ${list(kept.map((k) => k.display)) || 'Nothing'} ${kept.length > 1 ? 'are kept and still start' : 'is kept and still starts'}.`}</p></section>
    </div>
    ${needAck ? html`<${EraseNote} s=${s} e=${e}>
      <label class="ins-check"><input type="checkbox" id="erase-ack" checked=${ack} onChange=${(ev) => setAck(ev.target.checked)} /><span>I know that the files on ${s.m.the} that this plan erases will be gone${e.inBackup ? ', apart from the backup' : ''}.</span></label>
      <p><strong>Way out:</strong> go back to the plan or stop here. Nothing has changed yet.</p>
    </${EraseNote}>` : null}
    <section class="ins-stack" id="backup-place">
      <h2 class="ins-h2">Where the backup goes</h2>
      ${b.on ? html`<fieldset class="ins-stack" style="border:0;margin:0;padding:0" data-choice="backup-to">
        <legend class="il-label" style="margin-bottom:8px">${b.total} · read back before the first write</legend>
        ${targetOptions(s).map((t) => html`<label class=${'ins-choice' + (noRoom && t.value === b.target.value ? ' ins-card--warn' : '')}><input type="radio" name="backup-to" value=${t.value} checked=${t.value === b.target.value} onChange=${() => place(t.value)} />
          <span class="ins-choice__body"><span class="ins-choice__title">${t.title}</span><span class="ins-small">${t.body}</span></span></label>`)}
        <p class="ins-small ins-soft">${pc ? 'Disks GlassineOS goes on aren’t offered: a backup never goes to a disk it is a copy of.' : 'Your browser asks to let the installer save there.'} If the place is too small, the installer says so and you choose another.</p>
        ${noRoom ? html`<p class="ins-small"><strong>The backup doesn’t fit there.</strong> Choose a place with room for it, or go back to the plan.</p>` : null}
      </fieldset>` : html`<div class="ins-card ins-card--warn" id="backup-off"><span class="il-label">Your choice</span><span class="ins-card__title">No backup</span>
        <p class="ins-small">${b.consequence}</p>
        <p class="ins-small ins-soft">To back up after all, go back to the plan.</p></div>`}
    </section>
    ${word ? html`<section class="ins-stack">
      <h2 class="ins-h2">Confirm what is erased</h2>
      <label class="ins-field" for="osname">
        <span>${gone[0].display} ${e && e.withSystem ? 'and every file in it will be erased' : 'won’t start any more'}. To confirm, type its name: <strong>${word}</strong></span>
        <input id="osname" type="text" value=${typed} onInput=${(ev) => setTyped(ev.target.value)} autocomplete="off" aria-describedby="osname-state" />
        <span id="osname-state" class="ins-small ins-soft">${typed === '' ? 'Not typed yet.' : typedOk ? 'Matches.' : 'Doesn’t match yet.'}</span>
      </label>
    </section>` : null}
    <p class="ins-small ins-soft">You can stop at any time: the installer stops at the next safe point and offers to resume or undo.</p>
    <${Actions} next=${next} out="Go back to the plan, or stop here. Nothing has changed."
      buttons=${[
        { label: 'Install as planned', href: s.link('backup'), primary: true, disabled: !ok, id: 'install' },
        { label: 'Back to the plan', href: s.link('plan') },
        { label: 'Stop here', href: s.link('start'), push: true }]} />
  </${Page}>`;
}
