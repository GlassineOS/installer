// SPDX-License-Identifier: GPL-2.0-or-later
// Plan: every step in order with its escape, the choices and questions of the plan record, the
// backup, the space. A changed choice or answer asks for the plan of that input
// (records.plan); there may be none.

import { html, Admonition, Actions, Page, Owed, Steps, Table, saveJson } from '../lib/parts.js';
import { HARDWARE, hardwareName, cap, human, list } from '../lib/words.js';
import { stepViews, erase, spaceRows, splitSentence, systemView, goneSystems, stepList } from '../lib/model.js';
import { EraseNote } from './check.js';

const numbers = (plan, ids) => ids.map((id) => plan.steps.findIndex((x) => x.id === id) + 1).filter((n) => n > 0);

function Choice({ s, c }) {
  const T = s.desc ? s.desc.texts : { choices: {}, options: {} };
  const base = c.choice.split('.')[0];
  const sys = base === 'systems' ? s.plan.systems[Number(c.choice.split('.')[1]) - 1] : null;
  const legend = sys ? `${systemView(sys.system).display}, found on ${s.m.the}` : T.choices[base];
  const opts = Object.keys(T.options).filter((k) => k.startsWith(base + '.')).map((k) => k.slice(base.length + 1));
  const disk = base === 'target-disk' && s.report.machine.disks[0];
  const pick = (v) => s.withInput({ ...s.input, choices: { ...s.input.choices, [c.choice]: v } });
  const row = (v, text) => {
    const [title, body] = splitSentence(text);
    return html`<label class="ins-choice"><input type="radio" name=${'choice-' + c.choice} value=${v} checked=${c.value === v} onChange=${() => pick(v)} />
      <span class="ins-choice__body"><span class="ins-choice__title">${title}</span>${body ? html`<span class="ins-small">${body}</span>` : null}${c.value === v && c.default ? html`<span class="ins-small ins-soft">The default.</span>` : null}</span></label>`;
  };
  return html`<fieldset class="ins-stack" style="border:0;margin:0;padding:0" data-choice=${c.choice}>
    <legend class="il-label" style="margin-bottom:8px">${legend || html`<${Owed}>${c.choice}: the description has no text for this choice</${Owed}>`}</legend>
    ${sys && T.choices.systems ? html`<p class="ins-small">${T.choices.systems}</p>` : null}
    ${opts.length ? opts.map((v) => row(v, T.options[base + '.' + v]))
      : row(c.value, disk && c.value === 'first' ? `The first disk: ${human(disk.size)}, ${disk.kind.toUpperCase()}. ${disk.free ? human(disk.free) + ' of it is free.' : ''}` : c.value)}
    ${!opts.length ? html`<p class="ins-small ins-soft">No other ${base === 'target-disk' ? 'disk' : 'option'} was found.</p>` : null}
  </fieldset>`;
}

function Question({ s, q }) {
  const p = s.plan, hw = HARDWARE[q.hardware];
  const ns = numbers(p, q.steps);
  const way = p.steps.filter((x) => q.steps.includes(x.id)).map((x) => x.on_failure.way_out).find((w) => w && w.startsWith('transition '));
  const howText = way && s.desc && s.desc.texts.transitions[way.split(' ')[1]];
  const answer = (v) => s.withInput({ ...s.input, hardware: { ...s.input.hardware, [q.hardware]: v } });
  const name = hardwareName(q.hardware), article = /^[aeiou]/.test(name) ? 'an' : 'a';
  return html`<fieldset class="ins-stack" style="border:0;margin:0;padding:0" data-question=${q.hardware}>
    <legend class="il-label" style="margin-bottom:8px">Needed for ${stepList(ns)}</legend>
    <p class="ins-card__title">Do you have ${article} ${name} for ${s.m.the}?</p>
    <p class="ins-small">${hw ? hw.what + ' ' : ''}Only it gets ${s.m.the} out if ${stepList(ns)} ${ns.length === 1 ? 'goes' : 'go'} wrong.</p>
    ${howText ? html`<details class="il-details"><summary>How it is used</summary><p class="ins-small">${howText}</p></details>` : null}
    <div class="ins-grid">
      <label class="ins-choice"><input type="radio" name=${'hw-' + q.hardware} checked=${q.answer === true} onChange=${() => answer(true)} /><span class="ins-choice__body"><span class="ins-choice__title">Yes, I have one</span></span></label>
      <label class="ins-choice"><input type="radio" name=${'hw-' + q.hardware} checked=${q.answer === false} onChange=${() => answer(false)} /><span class="ins-choice__body"><span class="ins-choice__title">No</span></span></label>
    </div>
    ${q.answer === null ? html`<p class="ins-small ins-soft">Not answered yet.</p>` : null}
  </fieldset>`;
}

export function BackupChoice({ s }) {
  const sp = s.plan.space;
  return html`<fieldset class="ins-stack" style="border:0;margin:0;padding:0" id="backup">
    <legend class="il-label" style="margin-bottom:8px">The backup</legend>
    <label class="ins-choice"><input type="radio" name="backup" checked /><span class="ins-choice__body">
      <span class="ins-choice__title">Back up ${s.m.the} <span class="ins-tag ins-tag--soft">Recommended</span></span>
      <span class="ins-small">Everything the plan replaces or changes, read back to check it before anything is written. With it, the installer can repair damage and undo the installation.</span>
      <span class="ins-small"><strong>${human(sp.backup_bytes)}${sp.backup_unknown.length ? ' and more' : ''}.</strong> ${sp.backup_unknown.length ? `The size of ${list(sp.backup_unknown.map((x) => 'the ' + x))} isn’t known before it is read.` : ''}</span>
      <span class="ins-small ins-soft">${s.m.kind === 'pc' ? 'It goes to the USB stick, or to another disk you choose before the installation starts.' : 'It goes to a folder on this computer or on an external disk; you choose it on the next screen.'}</span>
    </span></label>
    <label class="ins-choice"><input type="radio" name="backup" disabled /><span class="ins-choice__body">
      <span class="ins-choice__title">Install without a backup</span>
      <span class="ins-small"><${Owed}>Not offered yet: the plan records have no backup choice</${Owed}></span>
    </span></label>
  </fieldset>`;
}

function Status({ s }) {
  const p = s.plan;
  if (p.status === 'refused') {
    const q = p.questions.find((x) => x.answer === false);
    return html`<${Admonition} kind="warning" seal="G" label="Can’t be installed" title="The plan stops here" id="refused">
      ${q ? html`<p>Without ${article(hardwareName(q.hardware))} ${hardwareName(q.hardware)}, ${s.m.the} can’t be installed: only it can undo a failed ${stepList(numbers(p, q.steps))}, and the installation can’t skip ${q.steps.length === 1 ? 'that step' : 'those steps'}. Nothing has changed.</p>` : null}
      ${q ? null : html`<p>The plan can’t be carried out on ${s.m.the}. Nothing has changed.</p>`}
      <details class="il-details"><summary>The plan’s reason</summary><ul class="ins-stack ins-small">${p.reasons.map((r) => html`<li>${cap(r)}.</li>`)}</ul></details>
      <p><strong>Way out:</strong> stop here, or change the answer above and make the plan again.</p>
    </${Admonition}>`;
  }
  if (p.status === 'developer-only') {
    return html`<${Admonition} kind="warning" seal="G" label="Not tried yet" title="For developers only, for now" id="developer-only">
      <p>${cap(stepList(numbers(p, p.untried_steps)))} write to ${s.m.the} and haven’t been tried on real hardware yet. Only a developer who asks for them may run them.</p>
      <details class="il-details"><summary>The plan’s reason</summary><ul class="ins-stack ins-small">${p.reasons.map((r) => html`<li>${r}</li>`)}</ul></details>
      <p><strong>Way out:</strong> stop here. A dry run can end with this plan; nothing was changed.</p>
    </${Admonition}>`;
  }
  if (p.status === 'needs-answers') {
    return html`<${Admonition} label="A question first" id="needs-answers"><p>Answer the question above: the plan depends on it.</p></${Admonition}>`;
  }
  return null;
}

const article = (w) => (/^[aeiou]/.test(w) ? 'an' : 'a');

export function NoRecord({ s, what }) {
  return html`<${Admonition} kind="warning" seal="G" label="No plan" title=${`There is no plan for ${what}`} id="no-record">
    <p>The installer has no plan record for this input yet: the core only plans it once it can run in this page.</p>
    <p><strong>Way out:</strong> <a class="il-link" href=${s.link(s.route, {}, { choices: {}, hardware: {} })}>go back to the default choices</a>. Nothing has changed.</p>
  </${Admonition}>`;
}

export function Plan({ s }) {
  const p = s.plan, pc = s.m.kind === 'pc';
  const title = pc ? 'The plan for this PC' : `The plan for your ${s.m.name || 'device'}`;
  if (!p) {
    return html`<${Page} s=${s} step="plan" eyebrow="Plan · nothing is written yet" title=${title}>
      ${s.report && !s.report.supported ? html`<p>${cap(s.m.the)} isn’t supported: there is no plan. <a class="il-link" href=${s.link('check')}>Back to the check</a>.</p>` : html`<${NoRecord} s=${s} what="these choices" />`}
      <${Actions} next="Nothing, until there is a plan." out="Stop here. Nothing was changed." buttons=${[{ label: 'Back to the check', href: s.link('check') }, { label: 'Stop here', href: s.link('start'), push: true }]} />
    </${Page}>`;
  }
  const refused = p.status === 'refused', ready = p.status === 'ready';
  const e = erase(s);
  const gone = goneSystems(p);
  const unchanged = pc ? 'the PC starts as before.' : 'the device starts its system as before.';
  return html`<${Page} s=${s} step="plan" eyebrow=${refused ? 'Plan · refused, nothing was changed' : 'Plan · nothing is written yet'} title=${title}
      lead=${refused ? `One answer decides whether ${s.m.the} can be installed.` : 'What the installer will do, in this order. Choose what to keep, then confirm on the next screen. Until then nothing is written.'}>
    ${e && !e.withSystem ? html`<${EraseNote} s=${s} e=${e} />` : null}
    <section class="ins-stack ins-stack--wide">
      <h2 class="ins-h2">Your choices</h2>
      ${p.choices.map((c) => html`<${Choice} s=${s} c=${c} />`)}
      ${gone.map((g) => html`<${Admonition} kind="danger" seal="!" label="Replaces a system" title=${`${g.display}: ${g.outcome}`}>
        <p>${e && e.withSystem ? `${g.display} and every file in it are erased.` : `${g.display} won’t start any more.`} On the next screen you confirm it by its name.</p>
        <p><strong>Way out:</strong> choose to keep it above. Until you confirm, nothing changes.</p>
      </${Admonition}>`)}
      ${p.questions.map((q) => html`<${Question} s=${s} q=${q} />`)}
      ${refused ? null : html`<${BackupChoice} s=${s} />`}
    </section>
    <${Status} s=${s} />
    ${refused ? null : html`
      <section class="ins-stack">
        <h2 class="ins-h2">The steps, in order</h2>
        <p class="ins-small ins-soft">Each step says where ${s.m.the} is if that step fails. Technical names are under Details.</p>
        <${Steps} views=${stepViews(s)} mode="plan" />
      </section>
      <section class="ins-stack"><${Table} head="Space it needs" rows=${spaceRows(s)} /></section>`}
    <${Actions}
      next=${refused ? 'Nothing more: the plan can’t go ahead without that step.' : ready ? 'Confirm: you see the whole plan once more and choose where the backup goes. Nothing is written before you confirm.' : p.status === 'needs-answers' ? 'Answer the question, then confirm.' : 'Nothing more: only a developer can go on with this plan.'}
      out=${'Stop here: a dry run can end with this plan. Nothing was changed; ' + unchanged}
      buttons=${[
        { label: 'Continue to confirm', href: s.link('confirm'), primary: true, disabled: !ready, id: 'confirm' },
        { label: 'Back to the check', href: s.link('check') },
        { label: refused ? 'Save the report' : 'Save the plan', onClick: () => saveJson(refused ? 'glassine-check-report.json' : 'glassine-plan.json', refused ? s.report : p) },
        { label: 'Stop here', href: s.link('start'), push: true }]} />
  </${Page}>`;
}
