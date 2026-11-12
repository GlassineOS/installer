// SPDX-License-Identifier: GPL-2.0-or-later
// Installed: GlassineOS is already on the machine; repair, reinstall or back to stock (the
// jobs of vocab JOBS). The installation record (version, date, backup) can't be read yet.

import { useState } from 'preact/hooks';
import { html, Actions, Page, Owed } from '../lib/parts.js';
import { JOB } from '../lib/words.js';

export function Installed({ s }) {
  const [what, setWhat] = useState('repair');
  const opts = Object.entries(JOB);
  const sel = JOB[what];
  return html`<${Page} s=${s} step="check" eyebrow="Check · done, nothing was changed" title="GlassineOS is already installed"
      lead=${html`<${Owed}>The version and the date: from the installation record, which can’t be read yet</${Owed}>`}>
    <div class="ins-card"><span class="il-label">The backup</span><p><${Owed}>Where it is: from the installation record</${Owed}></p><p class="ins-small ins-soft">Your browser asks you to choose its folder again before the installer can use it.</p></div>
    <section class="ins-stack">
      <h2 class="ins-h2">What do you want to do?</h2>
      ${opts.map(([id, o]) => html`<label class="ins-choice"><input type="radio" name="what" checked=${what === id} onChange=${() => setWhat(id)} /><span class="ins-choice__body">
        <span class="ins-choice__title">${o.title}</span><span class="ins-small">${o.body}</span></span></label>`)}
    </section>
    <${Actions} next=${`The plan for “${sel.title}”: every step and its escape, as for an installation. Nothing is written before you confirm.`}
      out="Close the installer. Nothing was changed."
      buttons=${[{ label: 'Make the plan', primary: true, disabled: true, id: 'plan' }, { label: 'Close', href: s.link('start'), push: true }]} />
    <p class="ins-small"><${Owed}>The plans for repair, reinstall and back to stock: the descriptions have no steps for these jobs yet</${Owed}></p>
  </${Page}>`;
}
