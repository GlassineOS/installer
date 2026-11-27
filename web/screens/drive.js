// SPDX-License-Identifier: GPL-2.0-or-later
// Drive: make the PC installer drive (spec 005). The newest published drive (lib/drive.js), the
// browser's own download of it, the check of the downloaded file on this computer, and how to
// write it with an imaging program and start the PC from it. The page writes no stick and sets
// nothing up on it; the writing and starting steps lead on only after the file matched.
// Reached from Start in any browser, also one without WebUSB (app.js).

import { useEffect, useRef, useState } from 'preact/hooks';
import { html, Admonition, Actions, Page, Tick, Bang } from '../lib/parts.js';
import { DRIVE as W, human } from '../lib/words.js';
import { findDrive, checkFile, systemOf, RELEASES_PAGE } from '../lib/drive.js';
import { Sha256 } from '../lib/sha256.js';

const sum = (h) => h.match(/.{1,8}/g).join(' ');
const mono = 'font-family: var(--font-mono); font-size: 13.5px; overflow-wrap: anywhere';

function Code({ id, line }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => { try { await navigator.clipboard.writeText(line); setCopied(true); } catch { /* the line stays selectable */ } };
  return html`<div class="il-code" id=${id}>
    <div class="il-code__head"><span class="il-label">${W.dd.name}</span><button class="il-btn" type="button" onClick=${copy}>${copied ? W.copied : W.copy}</button></div>
    <pre style="white-space: pre-wrap; overflow-wrap: anywhere; padding: 8px 12px; margin: 0">${line}</pre>
  </div>`;
}

// One step of the four: mark, title, body. state: todo | current | done (only the check is ever done).
function Step({ id, state, title, children }) {
  return html`<li data-state=${state} data-drive-step=${id}>
    <span class="ins-plan__mark" aria-hidden="true">${state === 'done' ? html`<${Tick} size=${16} />` : null}</span>
    <div class="ins-plan__head"><span class="ins-plan__title">${title}</span></div>
    <div class="ins-plan__body ins-stack" style="min-width: 0">${children}</div>
  </li>`;
}

function Write({ d, system, setSystem }) {
  const ddLine = d.packed ? `${W.dd.unpack[d.packed]} ${d.name} | sudo dd of=/dev/sdX bs=4M conv=fsync status=progress` : `sudo dd if=${d.name} of=/dev/sdX bs=4M conv=fsync status=progress`;
  return html`<p>${W.writeIntro}</p>
    <fieldset class="ins-row" style="border:0; padding:0; margin:0" id="drive-system">
      <legend class="il-label" style="padding:0; margin-bottom:8px">${W.system}</legend>
      ${Object.entries(W.systems).map(([k, name]) => html`<label class="ins-choice" style="padding: 8px 16px"><input type="radio" name="drive-system" value=${k} checked=${system === k} onChange=${() => setSystem(k)} /><span class="ins-choice__title">${name}</span></label>`)}
    </fieldset>
    <div class="ins-grid" id="drive-imagers">
      ${W.imagers.map((m) => html`<section class="ins-card" data-imager=${m.id}>
        <span class="ins-card__title">${m.name}</span>
        <ol class="ins-stack ins-small" style="list-style: decimal; padding-left: 20px">${m.steps.map((t) => html`<li>${t}</li>`)}</ol>
      </section>`)}
    </div>
    ${system === 'linux' ? html`<section class="ins-stack" id="drive-dd">
      <span class="ins-card__title">${W.dd.name}</span>
      <p class="ins-small">${W.dd.find}</p>
      <pre class="il-code" style=${'padding: 8px 12px; margin: 0; white-space: pre-wrap; ' + mono}>lsblk -d -o NAME,SIZE,MODEL,TRAN</pre>
      <p class="ins-small">${W.dd.write}</p>
      <${Code} id="drive-dd-line" line=${ddLine} />
    </section>` : null}`;
}

export function Drive({ s }) {
  const [list, setList] = useState({ state: 'looking' });
  const [tries, setTries] = useState(0);
  const [check, setCheck] = useState(null); // { name, size, read, result, sha256 }
  const [system, setSystem] = useState(() => systemOf());
  const input = useRef(null);
  const running = useRef(null);

  useEffect(() => {
    let alive = true;
    setList({ state: 'looking' });
    findDrive().then((r) => alive && setList(r), (e) => alive && setList({ state: 'error', kind: e.kind || 'offline' }));
    return () => { alive = false; };
  }, [tries]);
  useEffect(() => () => running.current && running.current.stop(), []);

  const d = list.state === 'found' ? list.drive : null;
  const pick = (ev) => {
    const f = ev.target.files && ev.target.files[0];
    ev.target.value = '';
    if (!f || !d) return;
    if (running.current) running.current.stop();
    const me = checkFile(f, d, (read) => { if (running.current === me) setCheck((c) => ({ ...c, read })); }, Sha256);
    running.current = me;
    setCheck({ name: f.name, size: f.size, read: 0, result: 'checking' });
    me.done.then(
      (r) => { if (running.current === me && r.result !== 'stopped') setCheck((c) => ({ ...c, ...r })); },
      () => { if (running.current === me) setCheck((c) => ({ ...c, result: 'unreadable' })); },
    ).finally(() => { if (running.current === me) running.current = null; });
  };

  const res = check && check.result;
  const match = res === 'match';
  const bad = res && !['checking', 'match'].includes(res);
  const pct = check && d ? Math.floor((100 * check.read) / d.size) : 0;
  const listed = html`<p>${W.listed} <a class="il-link" href=${RELEASES_PAGE} id="drive-releases">${RELEASES_PAGE.replace(/^https:\/\//, '')}</a></p>`;
  const date = d ? new Date(d.published).toISOString().slice(0, 10) : '';
  const gb = d && !d.packed ? Math.ceil(d.size / 1e9) : null;

  return html`<${Page} s=${null} step="start" eyebrow=${W.eyebrow} title=${W.title} lead=${W.lead}>
    ${list.state === 'looking' ? html`<p class="ins-soft" id="drive-looking">${W.looking}</p>` : null}
    ${list.state === 'error' ? html`<${Admonition} kind="warning" seal="G" label="Not read" title=${W[list.kind === 'refused' ? 'refused' : 'offline'].title} id="drive-error">
      <p>${W[list.kind === 'refused' ? 'refused' : 'offline'].body}</p>${listed}
      <div class="ins-buttons"><button type="button" class="il-btn" id="drive-again" onClick=${() => setTries((n) => n + 1)}>${W.again}</button></div>
    </${Admonition}>` : null}
    ${list.state === 'none' ? html`<${Admonition} kind="note" label="Not yet" title=${W.none.title} id="drive-none"><p>${W.none.body}</p>${listed}</${Admonition}>` : null}
    ${d ? html`
      <section class="ins-stack" id="drive-need">
        <h2 class="ins-h2">${W.need}</h2>
        <ul class="ins-stack ins-small"><li>${W.needStick(gb)}</li><li>${W.needPc}</li><li>${W.needSecureBoot}</li></ul>
      </section>
      <ol class="ins-plan" aria-label=${W.title}>
        <${Step} id="download" state="todo" title=${W.steps[0]}>
          <div class="ins-card" id="drive-release" data-tag=${d.tag}>
            <span class="ins-card__title">${W.release(d.base)}</span>
            <p class="ins-small ins-soft">${W.published(date)}</p>
            <p class="ins-small" style=${mono}>${d.name} · ${human(d.size)}</p>
            <div class="ins-buttons"><a class="il-btn ins-primary" id="drive-download" href=${d.url}>${W.download(human(d.size))}</a></div>
            <p class="ins-small ins-soft">${W.downloadNote}</p>
          </div>
          <p class="ins-small">${W.unreviewed}</p>
        </${Step}>
        <${Step} id="check" state=${match ? 'done' : 'current'} title=${W.steps[1]}>
          <p>${W.checkBody}</p>
          <input type="file" id="drive-file" ref=${input} onChange=${pick} hidden />
          ${check ? html`<div class=${match ? 'ins-card ins-card--strong' : bad ? 'ins-card ins-card--warn' : 'ins-card'} id="drive-check" data-result=${res}>
            ${res === 'checking' ? html`<span class="ins-card__title">${W.checking(pct)}</span>
              <div class="ins-bar" role="progressbar" aria-valuenow=${pct} aria-valuemin="0" aria-valuemax="100"><span style=${'width: ' + pct + '%'}></span></div>` : html`
              <div class="ins-row"><span class=${'ins-yn ' + (match ? 'ins-yn--yes' : 'ins-yn--no')} aria-hidden="true">${match ? html`<${Tick} />` : html`<${Bang} />`}</span><span class="ins-card__title">${W.result[res].name}</span></div>
              <p class="ins-small">${['short', 'long'].includes(res) ? W.result[res].body(human(check.size), human(d.size)) : W.result[res].body}</p>`}
            <p class="ins-small" style=${mono}>${check.name}</p>
            ${res === 'differs' ? html`<div class="il-table-wrap"><table class="il-table ins-small"><tbody>
              <tr><th scope="row" style="width:34%">${W.checksum}</th><td style=${mono}>${sum(check.sha256)}</td></tr>
              <tr><th scope="row">${W.publishedSum}</th><td style=${mono}>${sum(d.sha256)}</td></tr>
            </tbody></table></div>` : null}
            ${match ? html`<p class="ins-small ins-soft">${W.checksum} <span style=${mono}>${sum(check.sha256)}</span></p>` : null}
          </div>` : null}
          <div class="ins-buttons"><button type="button" class=${match ? 'il-btn' : 'il-btn ins-primary'} id="drive-choose" onClick=${() => input.current && input.current.click()}>${check ? W.chooseAgain : W.choose}</button></div>
        </${Step}>
        <${Step} id="write" state="todo" title=${W.steps[2]}>
          ${match ? html`<div class="ins-stack" id="drive-write"><${Write} d=${d} system=${system} setSystem=${setSystem} /></div>`
            : bad ? html`<${Admonition} kind="danger" seal="!" label=${W.dontWrite.label} title=${W.dontWrite.title} id="drive-dont-write"><p><strong>Way out:</strong> ${W.dontWrite.body}</p></${Admonition}>`
            : html`<p class="ins-soft" id="drive-check-first">${W.checkFirst}</p>`}
        </${Step}>
        <${Step} id="start" state="todo" title=${W.steps[3]}>
          ${match ? html`<div class="ins-stack" id="drive-start">
            <${Admonition} kind="warning" seal="G" label=${W.secureBoot.label} title=${W.secureBoot.title} id="drive-secure-boot"><p>${W.secureBoot.body}</p><p>${W.secureBoot.how}</p></${Admonition}>
            <ol class="ins-stack" style="list-style: decimal; padding-left: 20px">${W.start.map((t) => html`<li>${t}</li>`)}</ol>
          </div>` : html`<p class="ins-soft">${W.checkFirst}</p>`}
        </${Step}>
      </ol>` : null}
    <${Actions} next=${list.state === 'error' ? W.next.error : !d ? W.next.none : match ? W.next.match : bad ? W.next.bad : W.next.before} out=${W.out}
      buttons=${[{ label: W.back, href: s ? s.link('start') : '#/start', push: true }]} />
  </${Page}>`;
}
