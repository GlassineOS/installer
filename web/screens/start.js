// SPDX-License-Identifier: GPL-2.0-or-later
// Start: what the installer does, and where to begin (a device, or this PC in its live session).
// state: browser | live | nowebusb (also whenever the browser has no WebUSB). Outside the live
// session, a PC leads to Drive: making the USB stick (spec 005), which every browser can.

import { html, Admonition, Actions, Page } from '../lib/parts.js';
import { BROWSERS } from '../lib/words.js';
import { useDevice, DeviceCard } from '../lib/device.js';

export function Start({ s, state }) {
  const live = s ? s.live || state === 'live' : state === 'live';
  const noUsb = state === 'nowebusb';
  const connect = (kind) => (s ? s.connect(kind) : '#/start');
  const dev = useDevice(s);
  const drive = s ? s.link('drive') : '#/drive';
  return html`<${Page} s=${null} step="start" eyebrow="GlassineOS installer" title="Install GlassineOS"
      lead="The installer finds out what your machine is, shows you everything it would do, and does it only after you confirm. If anything goes wrong, it says how to get back.">
    ${noUsb ? null : html`<${DeviceCard} s=${s} dev=${dev} offer />`}
    ${noUsb ? html`<${Admonition} kind="warning" seal="G" label="This browser can’t reach devices" title="Open this page in a Chromium-based browser" id="nowebusb">
      <p>This browser can’t talk to devices over USB. These can: ${BROWSERS}, on a computer with Linux, Windows or macOS.</p>
      <p><strong>Way out:</strong> nothing has happened. Close this page.</p>
    </${Admonition}>
    <section class="ins-card ins-card--strong">
      <span class="ins-tagline">A PC</span>
      <h2 class="ins-h2">Install on a PC</h2>
      <p>This works in any browser: get the GlassineOS USB stick’s file for a PC, checked, and how to write it.</p>
      <div class="ins-buttons" style="margin-top:12px"><a id="go-drive" class="il-btn ins-primary" href=${drive}>Install on a PC</a></div>
    </section>` : html`
      <div class="ins-grid">
        <section class="ins-card ins-card--strong">
          <span class="ins-tagline">An e-ink device</span>
          <h2 class="ins-h2">Connect a device</h2>
          <p>For an e-ink device, connected to this computer.</p>
          <p class="il-label" style="margin-top:8px">You need</p>
          <ul class="ins-stack ins-small">
            <li>This computer, with a Chromium-based browser. ${live ? 'The browser of the GlassineOS USB stick is one.' : 'This one is.'}</li>
            <li>A USB cable that carries data, not only charge.</li>
          </ul>
          <div class="ins-buttons" style="margin-top:12px"><a id="go-device" class=${live ? 'il-btn' : 'il-btn ins-primary'} href=${connect('device')}>Connect a device</a></div>
        </section>
        <section class="ins-card ins-card--strong">
          <span class="ins-tagline">A PC</span>
          <h2 class="ins-h2">Install on this PC</h2>
          ${live ? html`
            <p>You started this PC from the GlassineOS USB stick. You need nothing more.</p>
            <div class="ins-buttons" style="margin-top:12px"><a id="go-pc" class="il-btn ins-primary" href=${connect('pc')}>Install on this PC</a></div>` : html`
            <p>A PC is installed from a GlassineOS USB stick started on that PC. Here you get the stick’s file, checked, and how to write it.</p>
            <div class="ins-buttons" style="margin-top:12px"><a id="go-drive" class="il-btn" href=${drive}>Install on a PC</a></div>`}
        </section>
      </div>
      ${s && s.liveDevice ? html`<p class="ins-small">An installation was cut off, or you stopped it? <a class="il-link" id="carry-on" href="#/resume">Carry on with an installation</a>: it resumes from its record, next to its backup.</p>` : null}
      <p class="ins-soft ins-small">Only want to know? The check and the plan change nothing. You can stop after either and keep the report.</p>`}
    <${Actions} next=${noUsb ? 'A device needs one of the browsers named. A PC’s USB stick works in this one.' : 'The installer finds out what your machine is. Nothing is written.'}
      out="Close this page. Nothing has happened."
      buttons=${noUsb ? [{ label: 'Copy this page’s address', onClick: () => navigator.clipboard && navigator.clipboard.writeText(location.href) }] : []} />
  </${Page}>`;
}
