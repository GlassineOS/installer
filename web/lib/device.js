// SPDX-License-Identifier: GPL-2.0-or-later
// The live device's fastboot session on the screens (records/index.js: device, restart). After
// the check the session stays open, so the page can restart the device: its boot loader only
// answers a fresh start, and once the page lets go of it, it stays in its USB mode until its
// power button is held. Wherever the person leaves the check (Check itself, Reach it again,
// Stop here, Start), Restart it comes first while the session is open.

import { useEffect, useState } from 'preact/hooks';
import { html, Admonition } from './parts.js';

// The live device's session, re-rendered when it changes; null with fixtures or none chosen.
export function useDevice(s) {
  const [dev, setDev] = useState(() => (s && s.device ? s.device() : null));
  useEffect(() => (s && s.onDevice ? s.onDevice(setDev) : undefined), [s]);
  return dev;
}

export const isOpen = (dev) => !!dev && dev.state === 'open';

// The button: first of the screen's actions while the session is open.
export const restartButton = (s, dev, primary = true) => ({ label: 'Restart it', onClick: () => s.restart(), primary, id: 'restart', disabled: !isOpen(dev) });

// The way-out line while the session is open.
export const LEFT_IN_USB_MODE = 'Without a restart it is left in its boot loader’s USB mode, and only holding its power button gets it out. Closing or reloading this page tries to restart it too, but may not reach it.';

// What became of the session; with `offer`, the open session too, with its own button (the
// screens whose actions are about something else: Start, Reach).
export function DeviceCard({ s, dev, offer }) {
  if (!dev) return null;
  const st = dev.state;
  if (st === 'open') {
    if (!offer) return null;
    return html`<${Admonition} kind="warning" seal="G" label="Still connected" title="The device from the check is still in its boot loader’s USB mode" id="device-open">
      <p>Restart it, and it starts its system again. Otherwise it is left in its USB mode, and only holding its power button gets it out. Closing or reloading this page tries to restart it too, but may not reach it. It can’t be read again before it is restarted.</p>
      <div class="ins-buttons"><button type="button" class="il-btn ins-primary" id="restart" onClick=${() => s.restart()}>Restart it</button></div>
    </${Admonition}>`;
  }
  const T = {
    restarting: ['Restarting', 'Restarting the device', 'The installer asked it to restart. It starts its system again.'],
    restarted: ['Restarted', 'The device is restarting', 'It starts its system again and is no longer connected to this page. To check it again, start it into its boot loader’s USB mode and choose it again.'],
    failed: ['Not restarted', 'The device didn’t confirm the restart', 'If it still shows its boot loader’s USB mode, hold its power button until it turns off, then turn it on again.'],
    unplugged: ['Unplugged', 'The device was unplugged in its boot loader’s USB mode', 'This page can’t restart it any more. Hold its power button until it turns off, then turn it on again: it starts its system.'],
    closed: null,
  }[st];
  if (!T) return null;
  return html`<${Admonition} kind=${st === 'failed' || st === 'unplugged' ? 'warning' : 'note'} seal=${st === 'failed' || st === 'unplugged' ? 'G' : undefined} label=${T[0]} title=${T[1]} id=${'device-' + st}>
    <p>${T[2]}</p>
    ${st === 'failed' && dev.error ? html`<p class="ins-small ins-soft">${dev.error}</p>` : null}
  </${Admonition}>`;
}
