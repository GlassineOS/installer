// SPDX-License-Identifier: GPL-2.0-or-later
// Resume: an installation that was cut off (spec 001 Scenario 4). at: the first step not done
// (1 or more). Where the machine is now is the plan's escape for a cut after the last step
// done; there is no installation record to read yet, so `at` stands in for it.

import { useState } from 'preact/hooks';
import { html, Actions, Page, Owed, Tick } from '../lib/parts.js';
import { ENDS } from '../lib/words.js';
import { stepViews } from '../lib/model.js';

export function Resume({ s }) {
  const p = s.plan;
  const [what, setWhat] = useState('resume');
  if (!p) return html`<${Page} s=${s} step="check" eyebrow="Check" title="No plan"><p><a class="il-link" href=${s.link('plan')}>Back to the plan</a></p></${Page}>`;
  const views = stepViews(s), n = views.length;
  const at = Math.min(Math.max(s.num('at', Math.min(3, n - 1)), 1), n - 1);
  const last = p.steps[at - 1], nxt = views[at];
  const title = (v) => (v.owed ? v.kind.replace(/-/g, ' ') : v.title.replace(/\.$/, ''));
  const now = last.on_interruption.leaves;
  const res = what === 'resume';
  return html`<${Page} s=${s} step="check" eyebrow="Check · a half-finished installation" title="This installation was cut off"
      lead=${`It was cut off before step ${at + 1}, ${title(nxt)}. ${at === 1 ? 'Step 1 is' : `Steps 1 to ${at} are`} complete.`}>
    <div class="ins-card"><span class="il-label">Right now</span><p>${views[at - 1].interruption}</p></div>
    <section class="ins-stack">
      <h2 class="ins-h2">How it ends</h2>
      <p class="ins-small ins-soft">Whatever cut it off, ${s.m.the} ends in one of three states:</p>
      <ul class="ins-caps">${ENDS.map((e) => {
        const here = e.leaves.includes(now);
        return html`<li data-now=${here ? 'yes' : undefined}><span class=${'ins-yn ' + (here ? 'ins-yn--wait' : 'ins-yn--yes')} aria-hidden="true">${here ? null : html`<${Tick} />`}</span>
          <span class="ins-caps__text"><span class="ins-caps__name">${e.name}</span><span class="ins-small ins-soft">${here ? 'This is the state now. Choose below.' : e.body}</span></span></li>`;
      })}</ul>
    </section>
    <section class="ins-stack">
      <h2 class="ins-h2">What to do</h2>
      <label class="ins-choice"><input type="radio" name="what" checked=${res} onChange=${() => setWhat('resume')} /><span class="ins-choice__body"><span class="ins-choice__title">Resume from step ${at + 1}, ${title(nxt)}</span><span class="ins-small">The steps already done stay done. The rest follows the plan you confirmed.</span></span></label>
      <label class="ins-choice"><input type="radio" name="what" checked=${!res} onChange=${() => setWhat('undo')} /><span class="ins-choice__body"><span class="ins-choice__title">Undo: put the backup back</span><span class="ins-small">${s.m.the.charAt(0).toUpperCase() + s.m.the.slice(1)} is as it was before the installation.</span></span></label>
      <div class="ins-card">
        <span class="il-label">The backup</span>
        <span class="ins-card__title"><${Owed}>Where it is: from the installation record, which can’t be read yet</${Owed}></span>
        <p class="ins-small ins-soft">Both need it. Your browser asks you to choose its folder again before the installer can use it.</p>
      </div>
      <details class="il-details"><summary>The record of this installation</summary>
        <p class="ins-small">The installer reads it from ${s.m.the} and from the backup folder: each step’s outcome as it completed, the plan you confirmed, the versions it used.</p>
      </details>
    </section>
    <${Actions} next=${res ? 'The installer checks the backup again, then carries on from the step that was cut off.' : 'The installer checks the backup again, then puts it back.'}
      out="Close the installer. It stays as it is now, and the installer offers the same choice the next time."
      buttons=${[{ label: res ? 'Resume' : 'Undo', href: res ? s.link('install', { state: 'midway', at }) : s.link('done', { state: 'stock' }), primary: true, id: 'go' }, { label: 'Close', href: s.link('start'), push: true }]} />
  </${Page}>`;
}
