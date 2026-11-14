// SPDX-License-Identifier: GPL-2.0-or-later
// The way out when the browser couldn't open or claim a device's USB mode (the core's error
// kinds permission and busy): why, in words for the system this page runs on, and what sets the
// computer up once. The setup is the core's host setup record (records.hostSetup(),
// core/src/host_setup.rs): on Linux the one line that installs its udev rules file, on Windows
// the USB ids that need the WinUSB driver. The words are HOST_ACCESS (lib/words.js).

import { useEffect, useRef, useState } from 'preact/hooks';
import { html } from './parts.js';
import { HOST_ACCESS } from './words.js';

// linux | windows | other: the system this page runs on. ?host=linux|windows|other in the page's
// own query sets it (for the screen tests).
export function hostPlatform() {
  const forced = new URLSearchParams(location.search).get('host');
  if (['linux', 'windows', 'other'].includes(forced)) return forced;
  const p = String((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '').toLowerCase();
  if (p.startsWith('win')) return 'windows';
  // Not Android or ChromeOS: their browsers open USB devices without a host setup.
  if (p.includes('linux') && !/android|cros|chrome os/.test(p + ' ' + navigator.userAgent.toLowerCase())) return 'linux';
  return 'other';
}

// On Linux the setup line is offered when it can help: the device wasn't allowed to open, or a
// kernel driver the setup lets go of is known to take the mode. A mode held by something else
// gets only what to close.
// kind: the core's error kind (permission | busy). usb: which of the record's USB ids the screen
// reached for: a function of a record entry ({ vendor_id, product_id, kinds, modes, ... }).
export function HostAccess({ s, kind, usb }) {
  const [rec, setRec] = useState(null);
  const [copied, setCopied] = useState(null);
  const lineRef = useRef(null);
  useEffect(() => { let alive = true; s.hostSetup().then((r) => alive && setRec(r), () => {}); return () => { alive = false; }; }, []);
  if (!rec || !['permission', 'busy'].includes(kind)) return null;
  const W = HOST_ACCESS, host = hostPlatform();
  const ids = rec.usb.filter(usb);
  const held = ids.some((u) => u.release_driver);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(rec.linux.command);
      setCopied('done');
    } catch {
      // No clipboard: select the line, for the person to copy.
      const r = document.createRange();
      r.selectNodeContents(lineRef.current);
      getSelection().removeAllRanges();
      getSelection().addRange(r);
      setCopied('selected');
    }
  };
  const idWords = ids.map((u) => `${u.vendor_id.toUpperCase()} ${u.product_id.toUpperCase()}`);
  return html`<div class="ins-stack" id="host-access" data-host=${host} data-kind=${kind}>
    <p>${W.cause[kind]}${host === 'linux' ? ' ' + (kind === 'permission' ? W.linux.permission : held ? W.linux.busyHeld : W.linux.busy) : ''}</p>
    ${host === 'linux' && kind === 'busy' && !held ? null : host === 'linux' ? html`
      <p>${W.linux.setup(held)}</p>
      <div class="il-code" id="host-setup">
        <div class="il-code__head"><span class="il-label">${W.linux.label}</span><button class="il-btn" type="button" id="copy-host-setup" onClick=${copy}>${copied === 'done' ? W.copied : copied === 'selected' ? W.selected : W.copy}</button></div>
        <pre style="white-space: pre-wrap; overflow-wrap: anywhere"><span ref=${lineRef} id="host-setup-line">${rec.linux.command}</span></pre>
      </div>
      <details class="ins-small"><summary>${W.linux.shows(rec.linux.path)}</summary><pre style="white-space: pre-wrap; overflow-wrap: anywhere" id="host-setup-rules">${rec.linux.rules}</pre></details>
      <p>${W.linux.after}</p>`
    : host === 'windows' ? html`<p id="winusb">${W.windows.setup(idWords)}</p><p>${W.windows.after}</p>`
    : html`<p>${W.other}</p>`}
  </div>`;
}
