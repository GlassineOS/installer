// SPDX-License-Identifier: GPL-2.0-or-later
// Back up: what the plan backs up, saved and then read back, part by part.
// state: saving | verifying | verified | failed; at: the item being saved or read (0-based);
// pct. There is no record of a running backup yet: these show the plan's items at that moment.

import { html, Admonition, Actions, Page, Tick } from '../lib/parts.js';
import { backupView } from '../lib/model.js';
import { BOOT_SETTING, human, cap } from '../lib/words.js';

// One item per disk image, then the boot settings kept outside the disks, then the values unique
// to the machine (captured whether or not the backup is on, FR-010).
export function backupItems(s) {
  const p = s.plan, b = backupView(s);
  const items = b.images.map((i) => ({ name: `${i.title}, whole`, size: i.size, tech: i.tech }));
  if (b.settings.length) items.push({ name: 'How it starts', size: '', tech: b.settings.map((x) => `${x.id} (${BOOT_SETTING[x.kind] || x.kind})`).join(', ') });
  if (p.keeps.unit_values.length) items.push({ name: `Values unique to ${s.m.the}`, size: '', tech: p.keeps.unit_values.join(', ') });
  return items;
}

export function Backup({ s, state }) {
  const p = s.plan;
  if (!p) return html`<${Page} s=${s} step="backup" eyebrow="Back up" title="No plan"><p><a class="il-link" href=${s.link('plan')}>Back to the plan</a></p></${Page}>`;
  const st = ['saving', 'verifying', 'verified', 'failed'].includes(state) ? state : 'saving';
  const b = backupView(s);
  const items = backupItems(s), n = items.length;
  const at = st === 'verified' ? n : Math.min(s.num('at', Math.floor(n / 2)), n - 1);
  const pct = s.num('pct', 40);
  const reading = st !== 'saving';
  const pc = s.m.kind === 'pc';
  const unchanged = pc ? 'Nothing on the PC has changed; it starts as before.' : 'Nothing on the device has changed; it starts its system as before.';
  const total = b.on ? b.total : 'No disk image';
  const rows = items.map((it, i) => {
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
  }[st];
  return html`<${Page} s=${s} step="backup" eyebrow=${T[0]} title=${T[1]} lead=${T[2]}>
    ${st === 'failed' ? html`<${Admonition} kind="warning" seal="G" label="The installation didn’t start" title="There isn’t room for the backup there" id="failed">
      <p>The place is full, or its disk was removed. Without a backup that reads back, the installer writes nothing.</p>
      <p><strong>Way out:</strong> ${unchanged} Choose another place with ${total} free, such as an external disk, and back up again, or go back to the plan and install without a backup.</p>
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
      <h2 class="ins-h2">${reading ? 'Read back, part by part' : 'Saved, part by part'}</h2>
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
      next=${st === 'failed' ? 'Nothing, until there is a backup that reads back.' : st === 'verified' ? 'The installation starts with step 2.' : 'When every part reads back, the installation starts by itself.'}
      out=${st === 'failed' ? 'Stop here. ' + unchanged : 'Stop now. ' + unchanged + ' The backup stays where it is.'}
      buttons=${st === 'failed'
        ? [{ label: 'Choose another place', href: s.link('confirm'), primary: true }, { label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]
        : st === 'verified' ? [{ label: 'Continue', href: s.link('install', { state: p.steps.some((x) => x.point_of_no_return) ? 'ponr' : 'midway' }), primary: true }]
        : [{ label: 'Stop', href: s.link('start') }]} />
  </${Page}>`;
}

export { cap };
