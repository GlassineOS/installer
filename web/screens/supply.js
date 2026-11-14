// SPDX-License-Identifier: GPL-2.0-or-later
// Supply: a file the backup needs that only the person can provide (spec 004 Scenarios 2–4).
// Before a file is picked, the guide: what it is, why GlassineOS can't provide it, what it must
// match and the name it is usually found under, all from the device description. Then the core's
// check of the picked file (records.suppliedCheck) and what may happen with it: a known file
// goes on, one that isn't known only after the person ticks the warning, every other is refused.
// A file the device refused in this page (Back up, state refused; 004 FR-006) is shown as refused
// by the device, whatever its check said, until another file is picked.
// state: nofile (the person has none). The file is read here, with the browser's File API, and
// kept in this page's memory only: it is never uploaded, and no page names a place to get one.

import { useRef, useState } from 'preact/hooks';
import { html, Admonition, Actions, Button, Owed, Page, Tick, Bang } from '../lib/parts.js';
import { cap, human, list } from '../lib/words.js';
import { suppliedFor, suppliedState, shortValue } from '../lib/model.js';
import { NoRecord } from './plan.js';

// The largest file the core checks (core/src/supplied.rs MAX_BYTES). A bigger one is not read
// whole: the core gets one byte more than it takes, and refuses it.
const MAX_BYTES = 32 << 20;

// "dbbeb416b8b1…" in groups of eight.
const sum = (h) => h.match(/.{1,8}/g).join(' ');

function ValueTable({ rows, head }) {
  return html`<div class="il-table-wrap"><table class="il-table ins-small">
    <thead><tr>${head.map((h) => html`<th>${h}</th>`)}</tr></thead>
    <tbody>${rows.map((r) => html`<tr>${r.map((x, i) => (i === 0 ? html`<td>${x}</td>` : html`<td style="font-family: var(--font-mono); font-size: 13.5px; overflow-wrap: anywhere">${x}</td>`))}</tr>`)}</tbody>
  </table></div>`;
}

export function Supply({ s, state }) {
  const p = s.plan;
  const [, redraw] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const [copied, setCopied] = useState(null);
  const input = useRef(null);
  const nameRef = useRef(null);
  const pc = s.m.kind === 'pc';
  const unchanged = pc ? 'the PC starts as before.' : 'the device starts its system as before.';
  if (!p) {
    return html`<${Page} s=${s} step="plan" eyebrow="Plan · nothing is written yet" title="A file for the backup"><${NoRecord} s=${s} what="these choices" />
      <${Actions} next="Nothing, until there is a plan." out="Stop here. Nothing was changed." buttons=${[{ label: 'Back to the plan', href: s.link('plan') }]} /></${Page}>`;
  }
  const { need, got, verdict: v, byDevice } = suppliedState(s);
  if (!need) {
    return html`<${Page} s=${s} step="plan" eyebrow="Plan · nothing is written yet" title="No file is needed" lead=${`The plan for ${s.m.the} needs no file from you${p.backup.choice === 'off' ? ': the backup is off' : ''}.`}>
      <${Actions} next="Confirm: you see the whole plan once more. Nothing is written before you confirm." out=${'Stop here. Nothing was changed; ' + unchanged}
        buttons=${[{ label: 'Continue to confirm', href: s.link('confirm'), primary: true, disabled: p.status !== 'ready' }, { label: 'Back to the plan', href: s.link('plan') }, { label: 'Stop here', href: s.link('start'), push: true }]} />
    </${Page}>`;
  }
  const c = got && got.check;
  // A file the device refused in this page (004 FR-006) is shown as that, whatever its check said.
  const st = state === 'nofile' ? 'nofile' : byDevice ? 'device-refused' : v || 'guide';
  const T = need.texts, the = s.m.the;
  // "chip and signing key": the values the route checks, named as the route names them.
  const keys = list(need.matches.map((m) => m.name.toLowerCase())) || 'values';
  const choose = () => input.current && input.current.click();
  const pick = async (ev) => {
    const f = ev.target.files && ev.target.files[0];
    if (!f) return;
    setBusy(true);
    setError(null);
    try {
      const bytes = new Uint8Array(await f.slice(0, MAX_BYTES + 1).arrayBuffer());
      const check = await s.suppliedCheck(s.desc.id, need.id, bytes);
      const sendable = (check.verdict === 'known' || check.verdict === 'fits-unknown') && !s.deviceRefused(s.machine, need.id, check.sha256);
      // Only a file that may be sent stays in memory; of any other, only its check.
      s.keepSupplied(s.machine, need.id, { name: f.name, size: f.size, bytes: sendable ? bytes : null, check, trusted: false });
    } catch (e) {
      s.forgetSupplied(s.machine, need.id);
      setError(e);
    }
    ev.target.value = '';
    setBusy(false);
    redraw((n) => n + 1);
  };
  const trust = (on) => { got.trusted = on; redraw((n) => n + 1); };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(need.usualName);
      setCopied('done');
    } catch {
      // No clipboard: select the name, for the person to copy.
      const r = document.createRange();
      r.selectNodeContents(nameRef.current);
      getSelection().removeAllRanges();
      getSelection().addRange(r);
      setCopied('selected');
    }
  };

  const differs = c && c.differs ? c.differs : [];
  const H = {
    guide: ['Plan · needed for the backup', need.title, `The backup needs one file that only you can provide. Nothing is sent to ${the} until the installer has checked it.`],
    known: ['Plan · the file is checked', `A known ${need.noun}`, `This exact file has worked on ${the} before${c && c.verified ? '' : ', in development'}. It is used for the backup, and kept only in this page.`],
    'fits-unknown': ['Plan · the file is checked', `This ${need.noun} isn’t a known one`, differs.length
      ? `It is a ${need.noun}, but not one of the files known to work on ${the}, and it names another ${keys} than ${the}’s.`
      : `It is made for ${the}’s ${keys}, but it isn’t one of the files known to work.`],
    'wrong-chip-or-key': ['Plan · the file is checked', `This file doesn’t fit ${the}`, c && c.reported
      ? `The device reports another ${keys} than ${the}’s description names. Nothing is sent to it.`
      : `The installer sends only a file made for ${the}’s ${keys}.`],
    'known-bad': ['Plan · the file is checked', `This file doesn’t work on ${the}`, 'It is known not to work there, so it is never sent.'],
    'device-refused': ['Plan · the file was refused', `${cap(the)} didn’t accept this file`, `When the backup started, ${the} didn’t accept this ${need.short}, or it didn’t answer. Nothing was read. Choose another file.`],
    'not-a-programmer': ['Plan · the file is checked', `This isn’t ${/^[aeiou]/.test(need.noun) ? 'an' : 'a'} ${need.noun}`, 'The installer doesn’t recognise it as one, so it is never sent.'],
    nofile: ['Plan · needed for the backup', `Without the ${need.noun}`, 'The backup can’t be made without it. You decide how to go on.'],
  }[st] || ['Plan · the file is checked', 'The file was checked', ''];
  const known = st === 'known', fits = st === 'fits-unknown';
  const refused = c && !known && !fits;
  const verdictLine = known ? `Known: it has worked on ${the}` : st === 'device-refused' ? `Refused by ${the}` : fits ? 'Not a known file' : st === 'known-bad' ? `Known not to work on ${the}` : st === 'wrong-chip-or-key' ? 'Made for another device' : `Not ${/^[aeiou]/.test(need.noun) ? 'an' : 'a'} ${need.noun}`;
  const row = (k, made, want, rep) => [k.name, made, want, ...(rep === undefined ? [] : [rep])];
  const val = (m, k) => (m && m[k] ? shortValue(m[k]) : 'Not readable');
  const toConfirm = { label: known ? 'Continue to confirm' : 'Use this file', href: s.link('confirm'), primary: true, id: 'use' };
  const stop = { label: 'Stop here', href: s.link('start'), push: true };
  const back = { label: 'Back to the plan', href: s.link('plan') };
  const buttons = known ? [toConfirm, back, stop]
    : fits ? [{ ...toConfirm, disabled: !got.trusted }, { label: 'Choose another file', onClick: choose }, stop]
    : refused ? [{ label: 'Choose another file', onClick: choose, primary: true, id: 'choose' }, back, stop]
    : st === 'nofile' ? [{ label: 'Stop here', href: s.link('start'), primary: true }, { label: 'Install without a backup', href: s.link('plan', {}, { ...s.input, choices: { ...s.input.choices, backup: 'off' } }), id: 'no-backup' }, { label: 'I found it: choose the file', href: s.link('supply'), id: 'found' }]
    : [{ label: 'Choose the file…', onClick: choose, primary: true, id: 'choose', disabled: busy }, { label: 'I don’t have it', href: s.link('supply', { state: 'nofile' }), id: 'nofile' }, back, stop];
  const next = known ? 'Confirm: you see the whole plan once more. The file is sent only when the backup starts.'
    : fits ? (got.trusted ? 'Confirm: you see the whole plan once more, with this file.' : 'Nothing, until you tick the box or choose another file.')
    : st === 'device-refused' ? 'Nothing, until you choose another file.'
    : refused ? 'Nothing, until you choose a file that fits.'
    : st === 'nofile' ? 'Nothing: the installation can’t go on with a backup until you have the file.'
    : 'Choose the file: the installer checks it on this computer and shows the result.';

  return html`<${Page} s=${s} step="plan" eyebrow=${H[0]} title=${H[1]} lead=${H[2]}>
    <input type="file" id="file-input" ref=${input} onChange=${pick} hidden />
    ${st === 'guide' ? html`
      <section class="ins-stack" id="guide">
        <h2 class="ins-h2">What it is</h2>
        <p>${T.what || html`<${Owed}>what it is: the description has no text for it</${Owed}>`}</p>
        <h2 class="ins-h2">Why GlassineOS can’t provide it</h2>
        <p>${T.why || html`<${Owed}>why: the description has no text for it</${Owed}>`}</p>
      </section>
      ${need.matches.length ? html`<section class="ins-stack" id="matches">
        <h2 class="ins-h2">What it must match</h2>
        <div class="il-table-wrap"><table class="il-table"><tbody>${need.matches.map((m) => html`<tr><td style="width:34%">${m.name}</td><td style="font-family: var(--font-mono); font-size: 14px; overflow-wrap: anywhere">${m.value}</td></tr>`)}</tbody></table></div>
      </section>` : null}
      <section class="ins-stack" id="find">
        <h2 class="ins-h2">How to find it</h2>
        ${need.usualName ? html`<div class="il-code" id="name">
          <div class="il-code__head"><span class="il-label">Its name usually starts with</span><button class="il-btn" type="button" id="copy" onClick=${copy}>${copied === 'done' ? 'Copied' : copied === 'selected' ? 'Selected: copy it' : 'Copy'}</button></div>
          <pre><span ref=${nameRef}>${need.usualName}</span></pre>
        </div>` : null}
        <p class="ins-small">${T.recognise || html`<${Owed}>how to recognise it: the description has no text for it</${Owed}>`}</p>
        <p class="ins-small ins-soft">The installer checks whatever file you choose before it sends anything to ${the}.</p>
      </section>` : null}
    ${st === 'nofile' ? html`<section class="ins-stack ins-stack--wide" id="nofile">
      <h2 class="ins-h2">Without it, two ways on</h2>
      <div class="ins-card"><span class="ins-card__title">Come back with the file</span>
        <p class="ins-small">Stop here. Nothing has changed: ${unchanged} When you have the file, start the installer again.</p></div>
      <div class="ins-card"><span class="ins-card__title">Install without a backup</span>
        <p class="ins-small">${p.backup.consequence}</p>
        <p class="ins-small ins-soft">The plan states what that means before you confirm it.</p></div>
    </section>` : html`<section class="ins-stack">
      <h2 class="ins-h2">The file</h2>
      <div class=${known ? 'ins-card ins-card--strong' : c ? 'ins-card ins-card--warn' : 'ins-card'} id="file" data-verdict=${c ? v : st} data-sha256=${c ? c.sha256 : null} data-refused=${byDevice ? 'device' : null}>
        ${c ? html`
          <div class="ins-row"><span class=${'ins-yn ' + (known ? 'ins-yn--yes' : 'ins-yn--no')} aria-hidden="true">${known ? html`<${Tick} />` : html`<${Bang} />`}</span><span class="ins-card__title">${verdictLine}</span></div>
          <p class="ins-small" style="font-family: var(--font-mono); font-size: 13.5px; overflow-wrap: anywhere">${got.name}</p>
          <p class="ins-small ins-soft">${human(got.size)}${c.size === got.size ? html` · checksum <span style="font-family: var(--font-mono); font-size: 13.5px; overflow-wrap: anywhere">${sum(c.sha256)}</span>` : null}</p>
          <p class="ins-small">${known ? `The same, byte for byte, as a file that has worked on ${the}${c.verified ? '' : ' in development'}.` : fits ? `A ${need.noun}, but not the same as any file known to work on ${the}.` : st === 'device-refused' ? `${cap(the)} refused it when the backup started. It isn’t sent again.` : 'It isn’t sent to the device.'}</p>`
        : html`<span class="ins-card__title">${busy ? 'Checking the file…' : 'No file chosen'}</span>
          <p class="ins-small ins-soft">Your browser opens a window to choose it on this computer. It isn’t uploaded anywhere.</p>`}
        <div class="ins-row"><${Button} label=${c ? 'Choose another file…' : 'Choose the file…'} onClick=${choose} disabled=${busy} id="choose-file" /></div>
      </div>
    </section>`}
    ${error ? html`<${Admonition} kind="warning" seal="G" label="Not checked" title="The file couldn’t be checked" id="check-error">
      <p>${String(error.message || error)}</p>
      <p><strong>Way out:</strong> choose another file. Nothing was sent; ${the} hasn’t changed.</p>
    </${Admonition}>` : null}
    ${fits ? html`<${Admonition} kind="danger" seal="!" label="Not a known file" title=${differs.length ? `It names another ${keys}, and nobody has tried it` : 'It fits, but nobody has tried this exact file'} id="unknown">
      <p>A ${need.short} can read and change all of ${the}’s storage. A changed or wrong file could damage ${the} or what is on it. A known file is the same, byte for byte, as one that has worked on ${the} before.</p>
      ${need.testKeys || differs.length ? html`<p>${cap(the)} runs programs signed with test keys, so the ${keys} a file names prove nothing either way. What this file names:</p>` : html`<p>What this file names:</p>`}
      <${ValueTable} head=${['', 'The file names', `${cap(the)} has`, '']} rows=${need.matches.map((m) => [m.name, val(c.values, m.key), shortValue(m.value), differs.includes(m.key) ? 'Differs' : 'The same'])} />
      <label class="ins-check"><input type="checkbox" id="trust" checked=${!!got.trusted} onChange=${(ev) => trust(ev.target.checked)} /><span>I trust where this file comes from. Send it to ${the} anyway.</span></label>
      <p><strong>Way out:</strong> choose another file, such as one whose checksum is known. Until you tick the box, nothing is sent.</p>
    </${Admonition}>` : null}
    ${st === 'wrong-chip-or-key' ? html`<${Admonition} kind="warning" seal="G" label="Not sent" title=${c.reported ? 'The device isn’t the one described' : 'This file is for another device'} id="wrong">
      <${ValueTable} head=${['', 'The file is for', `${cap(the)} needs`, ...(c.reported ? ['The device reports'] : [])]}
        rows=${need.matches.map((m) => row(m, val(c.made_for, m.key), val(c.want, m.key), c.reported ? val(c.reported, m.key) : undefined))} />
      <p><strong>Way out:</strong> ${c.reported ? `check that the device connected is ${the}, then choose a file for it.` : 'choose another file.'} Nothing was sent; ${the} hasn’t changed.</p>
    </${Admonition}>` : null}
    ${st === 'device-refused' ? html`<${Admonition} kind="warning" seal="G" label="Not sent again" title=${`Refused by ${the}`} id="device-refused">
      <p>Its check said it could be used, but ${the} didn’t accept this ${need.short}, or the ${need.short} didn’t answer the installer. A file counts as working only once ${the} has accepted it and it answers. The installer doesn’t send this file again: choosing it again gives the same answer.</p>
      <p><strong>Way out:</strong> choose another file. Nothing was read; ${the} hasn’t changed.</p>
    </${Admonition}>` : null}
    ${st === 'known-bad' ? html`<${Admonition} kind="warning" seal="G" label="Not sent" title=${`Known not to work on ${the}`} id="known-bad">
      <p>${c.reason}</p>
      <p><strong>Way out:</strong> choose another file. Nothing was sent; ${the} hasn’t changed.</p>
    </${Admonition}>` : null}
    ${st === 'not-a-programmer' ? html`<${Admonition} kind="warning" seal="G" label="Not sent" title=${`Not ${/^[aeiou]/.test(need.noun) ? 'an' : 'a'} ${need.noun}`} id="not-programmer">
      <p>The installer doesn’t recognise this file as ${/^[aeiou]/.test(need.noun) ? 'an' : 'a'} ${need.noun} for ${the}.</p>
      <details class="il-details"><summary>What the installer saw</summary><p class="ins-small">${cap(c.why)}.</p></details>
      <p><strong>Way out:</strong> choose another file. Nothing was sent; ${the} hasn’t changed.</p>
    </${Admonition}>` : null}
    <${Actions} next=${next} out=${'Stop here. Nothing has changed: ' + unchanged} buttons=${buttons} />
  </${Page}>`;
}
