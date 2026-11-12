// SPDX-License-Identifier: GPL-2.0-or-later
// Install: the plan's steps carried out one after another.
// state: midway | ponr | stopping | stopped; at: the step running (0-based), or for stopped the
// next one; pct. No record of a running installation exists yet: these show the plan's steps
// at that moment. Nothing is written.

import { html, Admonition, Actions, Page, Steps } from '../lib/parts.js';
import { stepViews } from '../lib/model.js';

export function Install({ s, state }) {
  const p = s.plan;
  if (!p) return html`<${Page} s=${s} step="install" eyebrow="Install" title="No plan"><p><a class="il-link" href=${s.link('plan')}>Back to the plan</a></p></${Page}>`;
  const st = ['midway', 'ponr', 'stopping', 'stopped'].includes(state) ? state : 'midway';
  const views = stepViews(s), n = views.length;
  const ponrAt = views.findIndex((v) => v.ponr);
  const dflt = st === 'ponr' && ponrAt >= 0 ? ponrAt : Math.min(Math.max(1, Math.floor(n / 2)), n - 1);
  const stopped = st === 'stopped';
  let at = Math.min(Math.max(s.num('at', dflt), stopped ? 1 : 0), stopped ? n : n - 1);
  // A stopped installation stopped after a step the plan names a safe stop.
  if (stopped && !p.steps[at - 1].safe_stop) at = p.steps.findIndex((x, i) => i >= at - 1 && x.safe_stop) + 1 || n;
  const cur = views[stopped ? at - 1 : at], nxt = views[stopped ? at : at + 1];
  const title = (v) => (v.owed ? v.kind.replace(/-/g, ' ') : v.title.replace(/\.$/, ''));
  const pct = s.num('pct', st === 'ponr' ? 70 : 45);
  // A stop waits for the first step from the running one on that the plan names a safe stop.
  const safeAt = p.steps.findIndex((x, i) => i >= (stopped ? at - 1 : at) && x.safe_stop);
  const safe = safeAt >= 0 ? views[safeAt] : null;
  const afterSafe = safe ? (safeAt === at ? 'after this step' : `after step ${safeAt + 1}, ${title(safe)}`) : 'at the end of the installation';
  const connected = s.m.kind === 'pc' ? 'Keep the PC plugged in and the USB stick in.' : `Keep ${s.m.the} connected and this page open.`;
  return html`<${Page} s=${s} step="install"
      eyebrow=${stopped ? 'Install · stopped' : st === 'stopping' ? `Install · stopping ${afterSafe}` : `Install · step ${at + 1} of ${n}`}
      title=${stopped ? 'The installation is stopped' : title(cur)}
      lead=${stopped ? 'It stopped where you asked, after a step the plan names as a safe point.' : cur.ponr ? 'This is the point of no return. After this step, the installation can only be finished or undone by the installer.' : 'The installer follows the plan you confirmed, one step after another.'}>
    ${!stopped ? html`<${Admonition} label="While it runs"><p>${connected} If the connection is lost anyway, start the installer again: it finds the installation and resumes or undoes it.</p></${Admonition}>` : html`
      <div class="ins-card ins-card--strong" id="stopped">
        <span class="il-label">Stopped at a safe point</span>
        <span class="ins-card__title">Stopped after step ${at}, ${title(cur)}</span>
        <p>${p.steps[at - 1].on_interruption ? stepViews(s)[at - 1].interruption : ''}</p>
        <p class="ins-small ins-soft">Resume carries on with the next step. Undo puts the backup back. Close leaves it as it is: the next time you start the installer, it offers both again.</p>
      </div>`}
    <${Steps} views=${views} mode="progress" at=${at} pct=${pct} stopped=${stopped} />
    <${Actions}
      next=${stopped ? (nxt ? `Your choice: resume with step ${at + 1}, ${title(nxt)}, or undo.` : 'Your choice: resume, or undo.') : nxt ? `Step ${at + 2}: ${title(nxt)}.` : 'The restart into GlassineOS.'}
      out=${stopped ? `Undo puts the backup back: ${s.m.the} is as it was before the installation.` : st === 'stopping' ? `The installer stops ${afterSafe}, then offers to resume or undo.` : `Stop at the next safe point: ${afterSafe}. Then resume or undo.`}
      buttons=${stopped
        ? [{ label: 'Resume', href: s.link('install', { state: 'midway', at }), primary: true }, { label: 'Undo: put the backup back', href: s.link('done', { state: 'stock' }) }, { label: 'Close', href: s.link('resume', { at }), push: true }]
        : [{ label: st === 'stopping' ? `Stopping ${afterSafe}…` : 'Stop at the next safe point', href: st === 'stopping' ? undefined : s.link('install', { state: 'stopping', at }), disabled: st === 'stopping', id: 'stop' },
          ...(nxt ? [] : [{ label: 'Continue', href: s.link('verify'), primary: true }])]} />
  </${Page}>`;
}
