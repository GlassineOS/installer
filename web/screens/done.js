// SPDX-License-Identifier: GPL-2.0-or-later
// Done: the first start was accepted (state installed), or the machine is back to how it came
// (state stock). What it says about the machine comes from the plan.

import { html, Actions, Page, Tick, Owed, Admonition } from '../lib/parts.js';
import { keptSystems } from '../lib/model.js';
import { cap, list } from '../lib/words.js';

export function Done({ s, state }) {
  const p = s.plan;
  if (!p) return html`<${Page} s=${s} step="done" eyebrow="Done" title="No plan"><p><a class="il-link" href=${s.link('plan')}>Back to the plan</a></p></${Page}>`;
  const stock = state === 'stock', pc = s.m.kind === 'pc', The = cap(s.m.the);
  const kept = keptSystems(p);
  const rows = stock
    ? [['Checked', `Everything the installer changed is back from the backup, checked against it. GlassineOS is gone.`], ['The backup', 'It isn’t needed any more. You can delete it.']]
    : [
      [`Next, on ${s.m.the}`, 'Set up GlassineOS: it asks for a language, a network and a password.'],
      ...(kept.length ? [['Still there', `${list(kept.map((k) => k.display))}: kept, and it still starts.`]] : []),
      ['The backup', `Keep it: with it ${s.m.the} can be repaired, or the installation undone.`],
      ['The record', `A record of this installation is on ${s.m.the} and next to the backup: the machine, the versions, the layout, its capabilities and where the backup is.`],
      ...(p.keeps.device_data.length ? [['Its device data', `${cap(list(p.keeps.device_data))}: kept on ${s.m.the}.`]] : []),
      ['Nothing left behind', pc ? 'Nothing was copied off this PC.' : 'The files GlassineOS needed from the device were deleted from this computer.'],
    ];
  return html`<${Page} s=${s} step="done" eyebrow=${stock ? 'Back to stock · done' : 'Verify · accepted'}
      title=${stock ? `${The} is as it was before GlassineOS` : `GlassineOS is installed on ${s.m.the}`}
      lead=${stock ? `The backup is back on ${s.m.the}.` : 'Its first start was accepted. The installation is complete.'}>
    <div class="ins-row"><span class="ins-tag ins-tag--fill">${stock ? 'Back to stock' : 'Installed'}</span></div>
    <ul class="ins-caps">${rows.map(([k, v]) => html`<li><span class="ins-yn ins-yn--yes" aria-hidden="true"><${Tick} /></span><span class="ins-caps__text"><span class="ins-caps__name">${k}</span><span>${v}</span></span></li>`)}</ul>
    ${stock ? html`<${Admonition} label="Note"><p>What changed on ${s.m.the} after the backup was made isn’t back. <${Owed}>Which files that leaves out: from the backup’s record</${Owed}></p></${Admonition}>` : null}
    <${Actions}
      next=${stock ? `Nothing. ${The} starts as before.` : `First-time setup on ${s.m.the}. The installer’s work is done.`}
      out=${pc ? 'Close the installer.' : `Close the installer and unplug ${s.m.the}.`}
      buttons=${[{ label: 'Close the installer', href: s.link('start'), primary: true }]} />
  </${Page}>`;
}
