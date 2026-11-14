// SPDX-License-Identifier: GPL-2.0-or-later
// Plan: in short what happens, the danger it carries, what the person needs (room, a file only
// they can supply, special hardware), the steps (each with its escape, folded), and the choices
// (folded unless one replaces a system or turns the backup off). A changed choice or answer asks
// for the plan of that input (records.plan); there may be none. When the backup needs a file
// only the person can supply (spec 004), the way on is Supply, then Confirm.

import { html, Admonition, Actions, Page, Owed, Steps, Tick, Bang, saveJson } from '../lib/parts.js';
import { HARDWARE, hardwareName, cap, human } from '../lib/words.js';
import { stepViews, erase, spaceRows, splitSentence, systemView, goneSystems, stepList, backupView, suppliedState } from '../lib/model.js';
import { EraseNote } from './check.js';

// The flow's own choices (the backup and its place), shown by BackupChoice and on Confirm.
const FLOW_CHOICES = ['backup', 'backup-to'];

// A pick inside the folded choices re-plans and re-renders the screen: the fold stays open for it.
let keepOpen = false;
const repick = (s, input) => { keepOpen = true; s.withInput(input); };

const numbers = (plan, ids) => ids.map((id) => plan.steps.findIndex((x) => x.id === id) + 1).filter((n) => n > 0);

function Choice({ s, c }) {
  const T = s.desc ? s.desc.texts : { choices: {}, options: {} };
  const base = c.choice.split('.')[0];
  const sys = base === 'systems' ? s.plan.systems[Number(c.choice.split('.')[1]) - 1] : null;
  const legend = sys ? `${systemView(sys.system).display}, found on ${s.m.the}` : T.choices[base];
  let opts = Object.keys(T.options).filter((k) => k.startsWith(base + '.')).map((k) => k.slice(base.length + 1));
  // The disk GlassineOS goes on: `first` (the core's default, the first fixed disk) or diskN.
  const disks = base === 'target-disk' ? s.report.machine.disks.map((d, i) => ({ ...d, i })).filter((d) => !d.removable) : [];
  const diskText = (d) => `${d.i === disks[0].i ? 'The first disk' : 'Disk ' + d.i}: ${human(d.size)}, ${d.kind.toUpperCase()}. ${d.free ? human(d.free) + ' of it is free.' : ''}`;
  const pick = (v) => repick(s, { ...s.input, choices: { ...s.input.choices, [c.choice]: v } });
  const row = (v, text) => {
    const [title, body] = splitSentence(text);
    return html`<label class="ins-choice"><input type="radio" name=${'choice-' + c.choice} value=${v} checked=${c.value === v} onChange=${() => pick(v)} />
      <span class="ins-choice__body"><span class="ins-choice__title">${title}</span>${body ? html`<span class="ins-small">${body}</span>` : null}${c.value === v && c.default ? html`<span class="ins-small ins-soft">The default.</span>` : null}</span></label>`;
  };
  return html`<fieldset class="ins-stack" style="border:0;margin:0;padding:0" data-choice=${c.choice}>
    <legend class="il-label" style="margin-bottom:8px">${legend || html`<${Owed}>${c.choice}: the description has no text for this choice</${Owed}>`}</legend>
    ${sys && T.choices.systems ? html`<p class="ins-small">${T.choices.systems}</p>` : null}
    ${disks.length ? disks.map((d, k) => row(k === 0 ? 'first' : `disk${d.i}`, diskText(d)))
      : opts.length ? opts.map((v) => row(v, T.options[base + '.' + v])) : row(c.value, c.value)}
    ${disks.length === 1 || (!disks.length && !opts.length) ? html`<p class="ins-small ins-soft">No other ${base === 'target-disk' ? 'disk' : 'option'} was found.</p>` : null}
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
  const b = backupView(s);
  const pick = (v) => repick(s, { ...s.input, choices: { ...s.input.choices, backup: v } });
  return html`<fieldset class="ins-stack" style="border:0;margin:0;padding:0" id="backup" data-choice="backup">
    <legend class="il-label" style="margin-bottom:8px">The backup</legend>
    <label class="ins-choice"><input type="radio" name="backup" value="full" checked=${b.on} onChange=${() => pick('full')} /><span class="ins-choice__body">
      <span class="ins-choice__title">Back up ${s.m.the} whole <span class="ins-tag ins-tag--soft">Recommended</span></span>
      <span class="ins-small">One image of each disk the plan writes to: its partition table and every partition, read back to check it before anything is written. With it, the installer can repair any damage and undo the installation.</span>
      ${b.on ? html`<ul class="ins-stack ins-small" id="backup-images">${b.images.map((i) => html`<li><strong>${i.title}: ${i.size}.</strong> ${cap(i.holds)}; ${i.stored}.${i.atMost ? ' How much of the disk is used is known only once it is read.' : ''}</li>`)}</ul>` : null}
      <span class="ins-small ins-soft">It goes to ${b.on ? b.target.title.charAt(0).toLowerCase() + b.target.title.slice(1) : s.m.kind === 'pc' ? 'the installer’s USB stick' : 'this computer'}; ${s.m.kind === 'pc' ? 'you can choose another disk' : 'you choose the folder'} on the next screen.</span>
      ${b.on && b.dflt ? html`<span class="ins-small ins-soft">The default.</span>` : null}
    </span></label>
    <label class="ins-choice"><input type="radio" name="backup" value="off" checked=${!b.on} onChange=${() => pick('off')} /><span class="ins-choice__body">
      <span class="ins-choice__title">Install without a backup</span>
      <span class="ins-small">${b.on ? b.consequence : 'No disk is imaged.'}</span>
      ${b.lost.length ? html`<span class="ins-small"><strong>Without it, ${stepList(b.lost)} can’t be undone.</strong></span>` : null}
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

// The option text of a choice's value, from the description; for a found system, with its name.
function optionText(s, c) {
  const T = s.desc ? s.desc.texts : { options: {} };
  const base = c.choice.split('.')[0];
  const text = T.options[`${base}.${c.value}`];
  if (!text || base !== 'systems') return text;
  const sys = s.plan.systems[Number(c.choice.split('.')[1]) - 1];
  return sys ? `${systemView(sys.system).display}: ${text.charAt(0).toLowerCase()}${text.slice(1)}` : text;
}

// "In short": plain sentences, one per outcome. They come from the description's own option texts
// (what each choice means on this machine) and the plan's backup; the description has no texts of
// their own for this card.
function inShort(s) {
  const p = s.plan, b = backupView(s), gone = goneSystems(p);
  const out = [];
  for (const c of p.choices.filter((x) => !FLOW_CHOICES.includes(x.choice) && x.choice !== 'target-disk')) {
    const text = optionText(s, c);
    const base = c.choice.split('.')[0];
    const sys = base === 'systems' ? p.systems[Number(c.choice.split('.')[1]) - 1] : null;
    const warn = sys ? !/^kept/.test(sys.outcome) : gone.length > 0;
    out.push({ text: text || html`<${Owed}>${c.choice}: the description has no text for this option</${Owed}>`, warn });
  }
  out.push(b.on
    ? { text: `First, a backup is made, to ${b.target.title.charAt(0).toLowerCase() + b.target.title.slice(1)}, and checked by reading it back. With it, ${s.m.the} can be put back as it was.`, warn: false }
    : { text: b.consequence, warn: true });
  return out;
}

// The summary line of the folded choices: the option chosen for each, then the backup.
function choiceLine(s) {
  const p = s.plan, b = backupView(s);
  const parts = p.choices.filter((x) => !FLOW_CHOICES.includes(x.choice)).map((c) => {
    if (c.choice === 'target-disk') return c.value === 'first' ? 'The first disk' : `Disk ${c.value.replace(/^disk/, '')}`;
    return splitSentence(optionText(s, c) || c.value)[0].replace(/\.$/, '');
  });
  parts.push(b.on ? 'With a backup' : 'No backup');
  return parts.join(' · ');
}

const NEED_NAME = { 'For the backup': 'Room for the backup', 'For what the installer downloads': 'Room for what the installer downloads' };

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
  const b = backupView(s);
  const { need, got, ok: fileOk } = suppliedState(s);
  const choices = p.choices.filter((c) => !FLOW_CHOICES.includes(c.choice));
  const openChoices = keepOpen || gone.length > 0 || (!refused && !b.on);
  keepOpen = false;
  const unchanged = pc ? 'the PC starts as before.' : 'the device starts its system as before.';
  const fileTag = !got ? ['Not chosen yet', 'ins-tag ins-tag--dashed'] : fileOk ? ['Chosen and checked', 'ins-tag'] : got.check.verdict === 'fits-unknown' ? ['Chosen, not a known file', 'ins-tag ins-tag--heavy'] : ['Chosen, doesn’t fit', 'ins-tag ins-tag--heavy'];
  return html`<${Page} s=${s} step="plan" eyebrow=${refused ? 'Plan · refused, nothing was changed' : 'Plan · nothing is written yet'} title=${title}
      lead=${refused ? `One answer decides whether ${s.m.the} can be installed.` : 'Read this once. The recommended choices are already made; nothing is written until you confirm.'}>
    ${refused ? null : html`<section class="ins-card ins-card--strong" id="short">
      <span class="il-label">In short</span>
      <ul class="ins-caps">${inShort(s).map((x) => html`<li><span class=${'ins-yn ins-yn--small ' + (x.warn ? 'ins-yn--no' : 'ins-yn--yes')} aria-hidden="true">${x.warn ? html`<${Bang} />` : html`<${Tick} size=${12} />`}</span><span class="ins-caps__text">${x.text}</span></li>`)}</ul>
    </section>`}
    ${e && !e.withSystem ? html`<${EraseNote} s=${s} e=${e} />` : null}
    ${gone.map((g) => html`<${Admonition} kind="danger" seal="!" label="Replaces a system" title=${`${g.display}: ${g.outcome}`}>
      <p>${e && e.withSystem ? `${g.display} and every file in it are erased.` : `${g.display} won’t start any more.`} On the next screen you confirm it by its name.</p>
      <p><strong>Way out:</strong> choose to keep it under Change the choices. Until you confirm, nothing changes.</p>
    </${Admonition}>`)}
    ${!refused && !b.on ? html`<${Admonition} kind="danger" seal="!" label="No backup" title="You chose to install without a backup" id="backup-off">
      <p>${b.consequence}</p>
      ${b.lost.length ? html`<p><strong>Without it, ${stepList(b.lost)} can’t be undone.</strong></p>` : null}
      <p><strong>Way out:</strong> choose the backup under Change the choices. Until you confirm, nothing changes.</p>
    </${Admonition}>` : null}
    <section class="ins-stack" id="need">
      <h2 class="ins-h2">What you need</h2>
      <ul class="ins-caps">
        ${refused ? null : spaceRows(s).map(([k, v]) => html`<li><span class="ins-caps__text"><span class="ins-caps__name">${NEED_NAME[k] || k}</span><span class="ins-small">${v}</span></span></li>`)}
        ${need ? html`<li id="supply"><span class="ins-caps__text"><span class="ins-row" style="gap:8px"><span class="ins-caps__name">${need.title}</span><span class=${fileTag[1]}>${fileTag[0]}</span></span>
          <span class="ins-small">A file only you can provide, for the backup. ${got ? html`<span style="overflow-wrap: anywhere">${got.name}.</span>` : 'The next screen helps you find it.'}</span></span></li>` : null}
        ${p.questions.map((q) => html`<li><span class="ins-caps__text"><${Question} s=${s} q=${q} /></span></li>`)}
      </ul>
    </section>
    <${Status} s=${s} />
    ${refused ? null : html`<section class="ins-stack" id="what-happens">
      <h2 class="ins-h2">What happens</h2>
      <${Steps} views=${stepViews(s)} mode="summary" />
      <details class="il-details" id="ifwrong"><summary>If a step goes wrong</summary>
        <p class="ins-small ins-soft">Each step, with where ${s.m.the} is if that step fails. Technical names are under Details.</p>
        <${Steps} views=${stepViews(s)} mode="plan" />
      </details>
    </section>`}
    ${choices.length || !refused ? html`<details class="il-details" id="choices" open=${openChoices}><summary>Change the choices · ${choiceLine(s)}</summary>
      <section class="ins-stack ins-stack--wide" style="margin-top:12px">
        ${choices.map((c) => html`<${Choice} s=${s} c=${c} />`)}
        ${refused ? null : html`<${BackupChoice} s=${s} />`}
      </section>
    </details>` : null}
    <${Actions}
      next=${refused ? 'Nothing more: the plan can’t go ahead without that step.'
        : ready && need ? 'Provide the file the backup needs: the installer checks it, then you confirm the whole plan. Nothing is written before you confirm.'
        : ready ? `Confirm: you see the whole plan once more${b.on ? ' and choose where the backup goes' : ''}. Nothing is written before you confirm.`
        : p.status === 'needs-answers' ? 'Answer the question, then confirm.' : 'Nothing more: only a developer can go on with this plan.'}
      out=${'Stop here: a dry run can end with this plan. Nothing was changed; ' + unchanged}
      buttons=${[
        need ? { label: 'Continue: provide the file', href: s.link('supply'), primary: true, disabled: !ready, id: 'confirm' }
          : { label: 'Continue to confirm', href: s.link('confirm'), primary: true, disabled: !ready, id: 'confirm' },
        { label: 'Back to the check', href: s.link('check') },
        { label: refused ? 'Save the report' : 'Save the plan', onClick: () => saveJson(refused ? 'glassine-check-report.json' : 'glassine-plan.json', refused ? s.report : p) },
        { label: 'Stop here', href: s.link('start'), push: true }]} />
  </${Page}>`;
}
