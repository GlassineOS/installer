// SPDX-License-Identifier: GPL-2.0-or-later
// Verify: the installation is done only once GlassineOS's first start has been accepted
// (spec 001 FR-013). state: waiting | notaccepted. What happens when a start isn't accepted
// comes from the plan's capabilities (fallback-boot, trial-boot); there is no record of a
// running start yet.

import { html, Admonition, Actions, Page, Owed, Tick } from '../lib/parts.js';

export function Verify({ s, state }) {
  const p = s.plan;
  if (!p) return html`<${Page} s=${s} step="verify" eyebrow="Verify" title="No plan"><p><a class="il-link" href=${s.link('plan')}>Back to the plan</a></p></${Page}>`;
  const failed = state === 'notaccepted', pc = s.m.kind === 'pc', undo = p.backup.choice !== 'off';
  const has = (id) => p.capabilities.some((c) => c.id === id && c.status === 'have');
  const fallback = has('fallback-boot'), trial = has('trial-boot');
  const back = fallback ? `${s.m.the.charAt(0).toUpperCase() + s.m.the.slice(1)} went back by itself to the start it had before.` : null;
  const P = [
    { title: 'Restart', body: `${pc ? 'The PC' : s.m.the.charAt(0).toUpperCase() + s.m.the.slice(1)} restarts into GlassineOS.` },
    { title: 'GlassineOS starts', body: trial ? 'Its first version starts on trial.' : 'Its first version starts.' },
    { title: 'The start is accepted', body: 'GlassineOS has run for about two minutes without a problem and marks this start as good. Only then is the installation done.' },
  ];
  const at = failed ? 1 : 2;
  return html`<${Page} s=${s} step="verify" eyebrow="Verify · the first start"
      title=${failed ? 'The first start didn’t work' : 'Waiting for GlassineOS to be accepted'}
      lead=${failed ? 'The installation isn’t done: GlassineOS is written, but its first start wasn’t accepted.' : 'GlassineOS is written. The installation counts as done only once its first start has been accepted.'}>
    <div class="ins-row"><span class="ins-tag ins-tag--dashed">Not finished yet</span><span class="ins-small ins-soft">${pc ? 'This screen comes back on GlassineOS’s first start and follows it there.' : `Keep ${s.m.the} connected. This page follows its start.`}</span></div>
    ${failed ? html`<${Admonition} kind="warning" seal="G" label="Not accepted" title="GlassineOS didn’t start properly" id="notaccepted">
      <p>${back || html`<${Owed}>Where it is now: the plan doesn’t say how a start that isn’t accepted ends</${Owed}>`} Nothing is lost: ${undo ? 'the backup and ' : ''}the written GlassineOS ${undo ? 'are' : 'is'} still there.</p>
      <p><strong>Way out:</strong> try again${undo ? ', or undo the installation' : ''}.</p>
    </${Admonition}>` : null}
    <ol class="ins-plan" aria-label="The first start">${P.map((ph, i) => {
      const st = i < at ? 'done' : i === at ? 'current' : 'todo';
      return html`<li data-state=${st}><span class="ins-plan__mark" aria-hidden="true">${st === 'done' ? html`<${Tick} size=${16} />` : null}</span>
        <div class="ins-plan__head"><span class="ins-plan__title">${ph.title}</span><span class=${'ins-tag' + (st === 'current' ? ' ins-tag--heavy' : st === 'todo' ? ' ins-tag--soft' : '')}>${st === 'done' ? 'Done' : st === 'current' ? (failed ? 'Not accepted' : 'Waiting, about two minutes') : 'Waiting'}</span></div>
        <p class="ins-plan__body ins-small">${ph.body}</p></li>`;
    })}</ol>
    <${Actions}
      next=${failed ? 'Try again restarts into GlassineOS once more.' : 'Once the start is accepted: done.'}
      out=${failed ? (undo ? `Undo puts the backup back: ${s.m.the} is as it was before the installation.` : 'Close the installer: GlassineOS stays written. The backup was off, so there is no undo.') : fallback ? `If the start isn’t accepted, ${s.m.the} goes back by itself; this page then offers to try again${undo ? ' or undo' : ''}.` : `If the start isn’t accepted, this page offers to try again${undo ? ' or undo' : ''}.`}
      buttons=${failed
        ? [{ label: 'Try again', href: s.link('verify'), primary: true }, ...(undo ? [{ label: 'Undo the installation', href: s.link('done', { state: 'stock' }) }] : [])]
        : []} />
  </${Page}>`;
}
