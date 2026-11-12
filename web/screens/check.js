// SPDX-License-Identifier: GPL-2.0-or-later
// Check: what the machine is, its state, what it can do with GlassineOS, what an installation
// would do. From the check report; the plan record, where there is one, for the details.
// The report decides the variant: supported, not supported (no description matches), signals
// that contradict, or a machine that has to be reached in another mode to be recognised.

import { html, Admonition, Actions, Page, Caps, CapRow, Table, Cross, Owed, saveJson } from '../lib/parts.js';
import { CAPABILITY, CAPABILITY_STATUS, STATE, CONNECTION, MODE, cap, list, human } from '../lib/words.js';
import { fate, erase, spaceRows, factRows, techRows, reportLines, stepList, systemView } from '../lib/model.js';

export function capView(c) {
  const w = CAPABILITY[c.id] || { name: c.id, body: '' };
  const mark = c.status === 'have' ? 'yes' : c.status === 'needs' ? 'wait' : 'no';
  return {
    mark, name: w.name, tag: CAPABILITY_STATUS[c.status] || c.status,
    tagCls: 'ins-tag' + (c.status === 'have' ? '' : c.status === 'lack' ? ' ins-tag--heavy' : ' ins-tag--dashed'),
    body: c.status === 'needs' && c.with ? `${w.body} It needs ${c.with}.` : w.body,
    untried: !c.tried,
  };
}

export const CapList = ({ caps }) => html`<${Caps}>${caps.map(capView).map((c) => html`<${CapRow} mark=${c.mark} name=${c.name} tag=${c.tag} tagCls=${c.tagCls}>
  <span class="ins-small ins-soft">${c.body}${c.untried ? ' Not tried on this machine yet.' : ''}</span></${CapRow}>`)}</${Caps}>`;

export function Fate({ f }) {
  const col = (kind, label, xs) => html`<section data-kind=${kind}><span class="il-label">${label}</span><ul class="ins-stack">${xs.map((x) => html`<li>${x}</li>`)}</ul></section>`;
  return html`<div class="ins-fate">${col('keep', 'Keep', f.keep)}${col('backup', 'Back up', f.backup)}${col('replace', 'Replace', f.replace)}${col('create', 'Create', f.create)}</div>`;
}

export const PartitionDetails = ({ plan }) => html`<details class="il-details"><summary>Every part of the storage the plan names</summary>
  <${Table} mono rows=${plan.partitions.map((x) => [x.partition, `${x.class}: ${x.actions.join(', ')}${x.size !== null ? ' · ' + human(x.size) : ''}${x.loses_files ? ' · files lost' : ''}`])} />
  <p class="ins-small ins-soft">${plan.untouched_partitions} other partitions are not touched.</p>
</details>`;

export function EraseNote({ s, e, extra, children }) {
  return html`<${Admonition} kind="danger" seal="!" label="Erases your files" title=${`Files on ${s.m.the} will be erased`} id="erase">
    <p>${cap(stepList(e.steps))} ${e.steps.length === 1 ? 'deletes' : 'delete'} everything on ${e.parts.length === 1 ? 'one part' : e.parts.length + ' parts'} of its storage${e.size ? ', ' + e.size : ''}.</p>
    <p>${e.inBackup ? 'They are in the backup: going back puts them back as they were.' : 'They are not in the backup. Copy what you want to keep to this computer first.'}</p>
    ${extra}
    ${children || html`<p><strong>Way out:</strong> copy what you want to keep first. Until you confirm, nothing changes.</p>`}
  </${Admonition}>`;
}

// The plan summary of the report, where there is no plan record.
function fateOfSummary(s) {
  const p = s.report.plan;
  return {
    keep: [...p.keeps.map((x) => `${systemView(x).display}: kept, and it still starts`), `${p.untouched_partitions} other parts of the storage, not touched`],
    backup: [`${p.backs_up.length} parts of the storage`],
    replace: p.replaces.length ? [`${p.replaces.length} parts of the storage, written anew`] : ['Nothing'],
    create: ['GlassineOS', `${p.creates.length} new parts of the storage`],
  };
}

function Supported({ s }) {
  const r = s.report, p = s.plan, st = STATE[r.recognition.state];
  const e = p && erase(s);
  return html`
    <section class="ins-stack">
      <h2 class="ins-h2">Its state now</h2>
      <div class="ins-card ins-card--strong"><span class="ins-card__title">${st ? st.name : r.recognition.state}</span><p>${st ? st.body : ''}</p></div>
      <details class="il-details"><summary>The states the installer tells apart</summary>
        <ul class="ins-caps ins-small">${Object.values(STATE).map((x) => html`<li><span class="ins-caps__text"><span class="ins-caps__name">${x.name}</span>${x.offer}</span></li>`)}</ul>
      </details>
    </section>
    <section class="ins-stack">
      <h2 class="ins-h2">What it can do with GlassineOS</h2>
      <${CapList} caps=${p ? p.capabilities : r.capabilities} />
    </section>
    <section class="ins-stack">
      <h2 class="ins-h2">What an installation would do</h2>
      <${Fate} f=${p ? fate(s) : fateOfSummary(s)} />
      ${p ? html`<${PartitionDetails} plan=${p} />` : null}
      ${e ? html`<${EraseNote} s=${s} e=${e} />` : null}
      ${p ? html`<${Table} head="Space it needs" rows=${spaceRows(s)} />` : html`<p class="ins-small"><${Owed}>The space it needs: there is no plan record for this machine yet</${Owed}></p>`}
    </section>
    <section class="ins-stack">
      <h2 class="ins-h2">How the installer reaches it from here</h2>
      <${Caps}>${(r.runs_on ? r.runs_on.connections : []).map((c) => html`<${CapRow} mark="yes" name=${(CONNECTION[c] || { name: c }).name}><span class="ins-small ins-soft">${(CONNECTION[c] || { note: '' }).note} ${MODE[r.mode] ? 'Now: ' + MODE[r.mode].charAt(0).toLowerCase() + MODE[r.mode].slice(1) + '.' : ''}</span></${CapRow}>`)}</${Caps}>
    </section>`;
}

function ShareReport({ s }) {
  const text = reportLines(s).join('\n');
  const json = JSON.stringify(s.report, null, 2);
  const save = () => saveJson('glassine-check-report.json', s.report);
  return html`<section class="ins-card ins-card--strong">
    <span class="ins-card__title">Share this report</span>
    <p class="ins-small">With it, the people who add devices to GlassineOS can see what this one would need. It holds no personal data and no value unique to this device: no serial number, no account, no files, no network address.</p>
    <pre class="ins-small" style="margin:8px 0 0; padding: 12px 16px; background: var(--sheet); color: var(--ink); border-radius: var(--v-ui-radius); font-family: var(--font-mono); font-size: 13px; line-height: 1.6; white-space: pre-wrap">${text}</pre>
    <div class="ins-buttons" style="margin-top:8px">
      <button type="button" class="il-btn" onClick=${() => navigator.clipboard && navigator.clipboard.writeText(json)}>Copy the report</button>
      <button type="button" class="il-btn" onClick=${save}>Save it as a file</button>
    </div>
  </section>`;
}

function NotSupported({ s }) {
  const r = s.report, rec = r.recognition;
  const signals = rec.signals;
  if (rec.result === 'conflict') {
    return html`<section class="ins-stack">
      <h2 class="ins-h2">What contradicts</h2>
      <${Caps}>${signals.map((g) => html`<${CapRow} mark=${g.result === 'true' ? 'yes' : 'no'} glyph=${html`<${Cross} />`} name=${g.result === 'true' ? 'Matches the description' : 'Doesn’t match the description'} tag=${g.result === 'true' ? 'Yes' : 'No'} tagCls=${'ins-tag' + (g.result === 'true' ? '' : ' ins-tag--heavy')}>
        <span class="ins-small ins-soft" style="font-family: var(--font-mono)">${g.test}${g.seen ? ' · seen: ' + g.seen : ''}</span></${CapRow}>`)}</${Caps}>
    </section><${ShareReport} s=${s} />`;
  }
  return html`<section class="ins-stack">
      <h2 class="ins-h2">What is missing</h2>
      <${Caps}>
        <${CapRow} mark="no" glyph=${html`<${Cross} />`} name="A device description" tag="No" tagCls="ins-tag ins-tag--heavy"><span class="ins-small ins-soft">No description matches this hardware, so the installer doesn’t know which parts of it it may write.</span></${CapRow}>
      </${Caps}>
      <details class="il-details"><summary>What the check compared</summary>
        <${Table} mono rows=${signals.map((g) => [g.test, `${g.result}${g.seen ? ' (seen: ' + g.seen + ')' : ''}`])} />
      </details>
    </section><${ShareReport} s=${s} />`;
}

// Recognised only once it is reached in another mode (or its state only there).
function OtherMode({ s }) {
  const rec = s.report.recognition, T = s.desc ? s.desc.texts : null;
  return html`<section class="ins-stack">
    <h2 class="ins-h2">What the installer needs</h2>
    <ul class="ins-caps">${rec.needs_modes.map((mo) => html`<${CapRow} mark="wait" name=${MODE[(s.desc && (s.desc.modes.find((m) => m.id === mo) || {}).kind) || mo] || mo}>
      <span class="ins-small">${T && T.modes[mo] ? T.modes[mo] : html`<${Owed}>How to reach it: the description has no text for it</${Owed}>`}</span></${CapRow}>`)}</ul>
    <details class="il-details"><summary>Why</summary><ul class="ins-stack ins-small">${s.report.reasons.map((x) => html`<li>${x}</li>`)}</ul></details>
  </section>`;
}

export function Check({ s }) {
  const r = s.report;
  if (!r) return html`<${Page} s=${s} step="check" eyebrow="Check" title="No machine yet"><p><a class="il-link" href=${s.link('start')}>Start again</a></p></${Page}>`;
  const rec = r.recognition, pc = s.m.kind === 'pc';
  const variant = r.supported ? 'supported' : rec.result === 'none' || rec.result === 'conflict' ? 'not' : 'mode';
  const name = s.m.name;
  const title = {
    supported: pc ? 'This PC can run GlassineOS' : `This is ${s.m.the}`,
    not: rec.result === 'conflict' ? 'This device doesn’t match its description' : 'GlassineOS doesn’t support this device yet',
    mode: rec.result === 'match' ? `The installer can’t tell what is on ${s.m.the} yet` : `${cap(s.m.the)} has to be restarted to be recognised`,
  }[variant];
  const lead = {
    supported: 'GlassineOS supports it. Here is what it is now, and what an installation would keep, back up, replace and create.',
    not: rec.result === 'conflict' ? `Some of what it reports matches ${name ? 'the ' + name : 'a description'}, some doesn’t. The installer goes by the hardware and treats it as unsupported. Nothing was changed.` : 'The check is done and nothing was changed. Here is what the installer found, and what is missing.',
    mode: `It answered in a mode where the installer can’t see everything it needs. ${rec.needs_modes.length ? 'Restart it into the mode below and check again.' : ''} Nothing was changed.`,
  }[variant];
  const unchanged = pc ? 'the PC starts as before.' : 'the device starts its system as before.';
  const facts = factRows(s), tech = techRows(s);
  return html`<${Page} s=${s} step="check" eyebrow=${variant === 'supported' ? 'Check · done, nothing was changed' : 'Check · nothing was changed'} title=${title} lead=${lead}>
    <section class="ins-stack">
      <h2 class="ins-h2">What it is</h2>
      <${Table} rows=${facts} />
      ${tech.length ? html`<details class="il-details"><summary>Technical details</summary><${Table} mono rows=${tech} /></details>` : null}
    </section>
    ${variant === 'supported' ? html`<${Supported} s=${s} />` : variant === 'not' ? html`<${NotSupported} s=${s} />` : html`<${OtherMode} s=${s} />`}
    <${Actions}
      next=${variant === 'supported' ? 'The plan: every step in order, with what you can choose. Still nothing is written.' : variant === 'mode' ? 'Restart it as above, then reach it again.' : 'Nothing more to do here.'}
      out=${variant === 'supported' ? 'Stop here: a dry run ends with this report. Nothing was changed; ' + unchanged : `Close the installer${pc ? '' : ' and unplug ' + s.m.the}. Nothing was changed.`}
      buttons=${variant === 'supported'
        ? [{ label: 'Make the plan', href: s.link('plan'), primary: true, id: 'plan' }, { label: 'Save this report', onClick: () => saveJson('glassine-check-report.json', r) }, { label: 'Stop here', href: s.link('start'), push: true }]
        : variant === 'mode' ? [{ label: 'Reach it again', href: s.link('reach'), primary: true }, { label: 'Stop here', href: s.link('start'), push: true }]
        : [{ label: 'Close the installer', href: s.link('start'), primary: true }]} />
  </${Page}>`;
}

export { list };
