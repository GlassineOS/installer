// SPDX-License-Identifier: GPL-2.0-or-later
// The parts every screen is built from: the header with the six steps, the step list, the
// action bar, and the small marks. Classes are Interleaf's (il-*) and the installer's (ins-*).

import { h } from 'preact';
import htm from 'htm';
import { FLOW } from './words.js';

export const html = htm.bind(h);

export const Tick = ({ size = 14 }) => html`<svg width=${size} height=${size} viewBox="0 0 24 24" style="fill:none;stroke-width:3;stroke-linecap:round;stroke-linejoin:round"><path d="M5 12.5l4.5 4.5L19 7.5"></path></svg>`;
export const Dash = () => html`<svg width="12" height="12" viewBox="0 0 24 24" style="fill:none;stroke-width:3;stroke-linecap:round"><path d="M6 12h12"></path></svg>`;
export const Cross = () => html`<svg width="12" height="12" viewBox="0 0 24 24" style="fill:none;stroke-width:3;stroke-linecap:round"><path d="M7 7l10 10M17 7 7 17"></path></svg>`;
export const Bang = () => html`<svg width="12" height="12" viewBox="0 0 24 24" style="fill:none;stroke-width:3;stroke-linecap:round"><path d="M12 6v8M12 18v.5"></path></svg>`;

// A text the records don't hold yet: shown as a dashed placeholder (the boards' "owed").
export const Owed = ({ children }) => html`<span class="ins-placeholder" data-owed="">${children}</span>`;

export function Admonition({ kind = 'note', label, seal, title, id, children }) {
  return html`<aside class=${'il-admonition il-admonition--' + kind} id=${id}>
    <span class="il-admonition__label">${seal ? html`<span class="il-admonition__seal" aria-hidden="true">${seal}</span>` : null}${label}</span>
    ${title ? html`<p class="il-admonition__title">${title}</p>` : null}
    ${children}
  </aside>`;
}

export function Brand() {
  return html`<span class="il-brand"><svg width="30" height="30" viewBox="0 0 48 48" aria-hidden="true"><rect x="2" y="4" width="30" height="34" rx="5" style="fill:none;stroke:currentColor;stroke-width:2.5"></rect><rect x="14" y="10" width="30" height="34" rx="5" transform="rotate(6 29 27)" style="fill:var(--highlight)"></rect><path d="M20 30 C 26 24, 30 34, 38 26" transform="rotate(6 29 27)" style="fill:none;stroke:var(--pen);stroke-width:3;stroke-linecap:round"></path></svg>GlassineOS <span class="ins-head__sub">installer</span></span>`;
}

// step: one of FLOW's ids, 'start' (no steps) or 'done' (all done).
export function Header({ step, chip }) {
  const at = FLOW.findIndex((f) => f.id === step);
  const all = step === 'done';
  return html`<div class="ins-headwrap">
    <header class="il-header ins-head">
      <${Brand} />
      ${chip ? html`<span class="ins-head__machine" title="The machine this installation is for">${chip}</span>` : null}
    </header>
    ${step === 'start' ? null : html`<ol class="ins-steps" aria-label="Steps of the installation">
      ${FLOW.map((f, i) => {
        const state = all || (at >= 0 && i < at) ? 'done' : i === at ? 'current' : 'todo';
        return html`<li data-state=${state} aria-current=${state === 'current' ? 'step' : undefined}>
          <span class="ins-steps__mark" aria-hidden="true">${state === 'done' ? html`<${Tick} />` : i + 1}</span>
          <span class="ins-steps__name">${f.name}</span>
          <span class="ins-steps__now">· step ${i + 1} of ${FLOW.length}</span>
        </li>`;
      })}
    </ol>`}
  </div>`;
}

// buttons: { label, href | onClick, primary, push, disabled, id }
export function Actions({ next, out, buttons = [] }) {
  return html`<div class="ins-actions">
    <dl class="ins-next">
      <dt class="il-label">Next</dt><dd>${next}</dd>
      <dt class="il-label">Way out</dt><dd>${out}</dd>
    </dl>
    ${buttons.length ? html`<div class="ins-buttons">${buttons.map((b, i) => html`<${Button} key=${i} ...${b} />`)}</div>` : null}
  </div>`;
}

export function Button({ label, href, onClick, primary, push, disabled, id }) {
  const cls = 'il-btn' + (primary ? ' ins-primary' : '') + (push ? ' ins-push' : '');
  if (href !== undefined && !disabled) return html`<a class=${cls} href=${href} id=${id}>${label}</a>`;
  return html`<button type="button" class=${cls} id=${id} disabled=${!!disabled} aria-disabled=${disabled ? 'true' : undefined} onClick=${onClick}>${label}</button>`;
}

// The page: header, then the sheet with the title block.
export function Page({ s, step, eyebrow, title, lead, children }) {
  return html`<div class="ins il-page">
    <${Header} step=${step} chip=${s && s.m ? s.m.chip : null} />
    <main class="ins-desk il-desk">
      <div class="il-sheet ins-sheet">
        <div class="il-title-block">
          <span class="il-label">${eyebrow}</span>
          <h1 class="il-h1">${title}</h1>
          ${lead ? html`<p class="il-lead">${lead}</p>` : null}
        </div>
        ${children}
      </div>
    </main>
  </div>`;
}

const Mark = ({ done }) => html`<span class="ins-plan__mark" aria-hidden="true">${done ? html`<${Tick} size=${16} />` : null}</span>`;

function Title({ v }) {
  return v.owed ? html`<${Owed}>${v.title}: the description has no text for it</${Owed}>` : v.title;
}

// The step list: mode plan (everything), summary (titles and tags), progress (state per step).
// views: from model.stepViews; at: the step running (progress); pct; stopped.
export function Steps({ views, mode = 'plan', at = -1, pct = 0, stopped = false, barNote }) {
  const prog = mode === 'progress';
  return html`<ol class="ins-plan" aria-label=${prog ? 'Progress' : 'The steps of the plan'}>
    ${views.map((v, i) => {
      const state = !prog ? 'todo-plain' : i < at ? 'done' : i === at && !stopped ? 'current' : 'todo';
      const cur = state === 'current';
      const full = mode === 'plan' || cur;
      const status = !prog ? null : state === 'done' ? (stopped && i === at - 1 ? 'Done · stopped here' : 'Done') : cur ? 'Now' : stopped && i === at ? 'Next' : 'Waiting';
      return html`<li data-state=${state} data-step=${v.id}>
        <${Mark} done=${state === 'done'} />
        <div class="ins-plan__head">
          <span class="ins-plan__title"><${Title} v=${v} /></span>
          ${v.ponr ? html`<span class="ins-tag ins-tag--heavy">Point of no return</span>` : null}
          ${v.erase ? html`<span class="ins-tag ins-tag--fill">Erases files</span>` : null}
          ${v.untried && mode !== 'progress' ? html`<span class="ins-tag ins-tag--dashed">Not tried on hardware yet</span>` : null}
          ${status ? html`<span class="ins-tag ins-tag--soft">${status}</span>` : null}
        </div>
        ${full && v.body ? html`<p class="ins-plan__body">${v.body}</p>` : null}
        ${cur ? html`<div><div class="ins-bar" role="progressbar" aria-valuenow=${pct} aria-valuemin="0" aria-valuemax="100"><span style=${'width: ' + pct + '%'}></span></div><p class="ins-small ins-soft" style="margin-top:4px">${pct} %${barNote ? ' · ' + barNote : ''}</p></div>` : null}
        ${cur && v.ponr ? html`<p class="ins-escape ins-escape--cable" style="margin-top:12px"><b>From here on</b> a stop leaves the installation for the installer to finish or undo. It can’t be left half-way on its own.</p>` : null}
        ${mode !== 'summary' && (!prog || cur) ? html`<p class=${'ins-escape' + (v.cable ? ' ins-escape--cable' : '')}><b>${cur ? 'If this step fails now:' : 'If this step fails:'}</b> ${v.escape}</p>` : null}
        ${full ? html`<details class="il-details"><summary>Details</summary>
          <p class="ins-small"><b>If it’s cut off after it:</b> ${v.interruption}</p>
          <ul class="ins-stack ins-small ins-soft" style="font-family: var(--font-mono); font-size: 13.5px; margin-top: 8px">${v.tech.map((t) => html`<li>${t}</li>`)}</ul>
        </details>` : null}
      </li>`;
    })}
  </ol>`;
}

export const Caps = ({ children }) => html`<ul class="ins-caps">${children}</ul>`;

// A yes / no / waiting row: mark is 'yes' | 'no' | 'wait'.
export function CapRow({ mark, name, tag, tagCls = 'ins-tag', children, glyph }) {
  return html`<li><span class=${'ins-yn ins-yn--' + mark} aria-hidden="true">${mark === 'yes' ? html`<${Tick} />` : glyph || (mark === 'no' ? html`<${Dash} />` : null)}</span>
    <span class="ins-caps__text">${tag ? html`<span class="ins-row" style="gap:8px"><span class="ins-caps__name">${name}</span><span class=${tagCls}>${tag}</span></span>` : html`<span class="ins-caps__name">${name}</span>`}${children}</span></li>`;
}

export function Table({ head, rows, mono }) {
  return html`<div class="il-table-wrap"><table class=${'il-table' + (mono ? ' ins-small' : '')}>
    ${head ? html`<thead><tr><th>${head}</th><th></th></tr></thead>` : null}
    <tbody>${rows.map(([k, v]) => html`<tr>${head ? html`<td style="width:34%">${k}</td>` : html`<th scope="row" style="width:34%">${k}</th>`}<td style=${mono ? 'font-family: var(--font-mono); font-size: 14px' : undefined}>${v}</td></tr>`)}</tbody>
  </table></div>`;
}

// A record saved as a file on this computer (never to a device).
export function saveJson(name, record) {
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([JSON.stringify(record, null, 2) + '\n'], { type: 'application/json' }));
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}
