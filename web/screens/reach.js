// SPDX-License-Identifier: GPL-2.0-or-later
// Reach: getting the machine to where the installer can check it.
// state: guide | hand | debugging | chooser | waiting | found | notfresh (a PC: always found).
// The ways in come from the description: its Android-debugging mode, the installer's
// transition from there into the boot loader's USB mode, and any way by hand into that mode.
// Live (the core in the browser), a device has no machine yet: the person picks it in the
// browser's chooser and the core inspects it, every time (LiveReach; state: chooser | notfresh).

import { useState } from 'preact/hooks';
import { html, Admonition, Actions, Page, Owed, CapRow, Caps, Tick, Bang } from '../lib/parts.js';
import { cap } from '../lib/words.js';

function ways(desc) {
  if (!desc) return {};
  const mode = (id) => desc.modes.find((m) => m.id === id);
  const adb = desc.modes.find((m) => m.kind === 'android-adb');
  const auto = adb && desc.transitions.find((t) => t.from === adb.id && t.by === 'installer' && mode(t.to) && mode(t.to).kind === 'fastboot');
  const hand = auto && desc.transitions.find((t) => t.to === auto.to && t.by === 'hand');
  const T = desc.texts;
  return {
    adb, auto,
    autoText: auto && T.modes[auto.to],
    handText: hand && T.transitions[hand.id],
    debugText: adb && T.modes[adb.id],
  };
}

const ChooserPicture = ({ the, name }) => html`<div class="ins-grid" style="align-items:start">
  <div class="ins-stack">
    <h2 class="ins-h2">What you’ll see</h2>
    <p>Your browser opens a small window of its own, listing the USB devices it can see. Choose ${the} and click <strong>Connect</strong>.</p>
    <p class="ins-soft ins-small">It may be listed under another name. The installer can only reach a device you choose there, and only from this page.</p>
    <p class="ins-soft ins-small">The list is empty? Click Cancel; the installer helps you find out why.</p>
  </div>
  <div class="ins-stack">
    <span class="il-label">The browser’s window (it looks different in each browser)</span>
    <div class="ins-chooser" aria-hidden="true">
      <span>This page wants to connect</span>
      <span class="ins-chooser__row">${name}</span>
      <span class="ins-chooser__buttons"><span>Cancel</span><span>Connect</span></span>
    </div>
  </div>
</div>`;

// The core's failures of a live inspect, in words (records.connectDevice; not-fresh has a state).
const FAILED = {
  'no-device': ['No device was chosen', 'The browser’s window was closed without a device, or it listed none. A device is listed only in its boot loader’s USB mode, connected with a cable that carries data.'],
  'several-devices': ['More than one device', 'Connect only the device to check.'],
  busy: ['The device is in use', 'Another program or browser tab is using it. Close it, then choose the device again.'],
  permission: ['Not allowed to open the device', 'The computer didn’t let the browser open it.'],
  timeout: ['The device stopped answering', 'Unplug it, start it again into its boot loader’s USB mode, and choose it again.'],
};

function LiveReach({ s, state }) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(null);
  const st = state === 'notfresh' ? 'notfresh' : 'chooser';
  const choose = async () => {
    setBusy(true); setFailed(null);
    try {
      const machine = await s.connectDevice();
      location.hash = s.link('check', {}, { choices: {}, hardware: {} }, machine);
    } catch (e) {
      console.info('inspect:', e.kind, e.message);
      setBusy(false);
      if (e.kind === 'not-fresh' && st !== 'notfresh') location.hash = s.link('reach', { state: 'notfresh' });
      else setFailed(e);
    }
  };
  const [title, body] = failed ? FAILED[failed.kind] || ['The device couldn’t be read', failed.message] : [];
  const T = {
    chooser: ['Your browser asks which device', 'Start the device into its boot loader’s USB mode and connect it. Then choose it in your browser’s window; the installer reads it at once.'],
    notfresh: ['The device needs a fresh restart', 'Almost: the installer needs it freshly started into its boot loader’s USB mode.'],
  }[st];
  return html`<${Page} s=${{ ...s, m: null }} step="check" eyebrow="Check · reach the device" title=${T[0]} lead=${T[1]}>
    ${st === 'notfresh' ? html`<${Admonition} kind="warning" seal="G" label="Start it again" title="Not freshly started">
      <p>The device answered, but it wasn’t started freshly into its boot loader’s USB mode. It has to be, for the check. Nothing was changed.</p>
      <p><strong>Way out:</strong> turn it off, start it again into its boot loader’s USB mode, keep it connected, and choose it again. Or stop here: it starts its system when you turn it off and on.</p>
    </${Admonition}>` : html`<${ChooserPicture} the="your device" name="Your device" />`}
    ${busy ? html`<div class="ins-card ins-card--strong" id="reading">
      <div class="ins-row"><span class="ins-yn ins-yn--wait" aria-hidden="true"></span><span class="ins-card__title">Reading the device</span></div>
      <p class="ins-small ins-soft">The installer asks the device what it is. It only reads.</p>
    </div>` : null}
    ${failed ? html`<${Admonition} kind="warning" seal="G" label="Not read" title=${title} id="inspect-failed">
      <p>${body}</p>
      ${title !== 'The device couldn’t be read' ? html`<p class="ins-small ins-soft">${failed.message}</p>` : null}
      <p><strong>Way out:</strong> nothing has changed. Choose the device again, or stop here.</p>
    </${Admonition}>` : null}
    <${Actions} next="After you choose, the check starts. It only reads."
      out="Stop here and unplug the device. Nothing has changed; it starts its system when you turn it off and on."
      buttons=${[{ label: st === 'notfresh' || failed ? 'Choose the device again' : 'Choose the device', onClick: choose, primary: true, id: 'choose', disabled: busy },
        { label: 'Stop here', href: s.link('start'), push: true }]} />
  </${Page}>`;
}

export function Reach({ s, state }) {
  if (s.liveDevice && s.m.kind !== 'pc') return html`<${LiveReach} s=${s} state=${state} />`;
  const pc = s.m.kind === 'pc';
  const w = ways(s.desc);
  const st = pc || !w.adb ? 'found' : ['guide', 'hand', 'debugging', 'chooser', 'waiting', 'found', 'notfresh'].includes(state) ? state : 'guide';
  const The = cap(s.m.the), the = s.m.the;
  const go = (to) => s.link('reach', { state: to });
  const stop = { label: 'Stop here', href: s.link('start'), push: true };
  const T = {
    guide: ['Check · reach the device', `Get ${the} ready`, `The installer checks ${the} in its boot loader’s USB mode, a mode it starts in before its system.`],
    hand: ['Check · reach the device', 'USB debugging needs to be on', `The installer reaches ${the} through its system’s USB debugging. Turn it on once, on the device.`],
    debugging: ['Check · reach the device', 'Turn on USB debugging', `Then the installer can restart ${the} into its boot loader’s USB mode for you. You do this once, on the device.`],
    chooser: ['Check · reach the device', 'Your browser asks which device', `Before the installer can talk to ${the}, your browser asks you to choose it. That window comes next.`],
    waiting: ['Check · reach the device', `Looking for ${the}`, 'Connect it in its boot loader’s USB mode. This page notices it by itself.'],
    found: ['Check · reach the machine', pc ? 'Nothing to connect' : 'Found it', pc ? 'The installer runs on this PC and reads it directly. There is nothing to connect.' : `${The} is connected and ready to be checked.`],
    notfresh: ['Check · reach the device', `${The} needs a fresh restart`, 'Almost: the installer needs it freshly started into its boot loader’s USB mode.'],
  }[st];
  const next = {
    guide: `The installer restarts ${the}; your browser asks you to choose it, twice.`,
    hand: `Once USB debugging is on, the installer restarts ${the} for you.`,
    debugging: `Back to getting ${the} ready; the installer restarts it for you.`,
    chooser: 'The browser’s window opens. After you choose, the check starts. It only reads.',
    waiting: 'Once it’s found, the check starts by itself.',
    found: 'The check: what the machine is and what an installation would do. It only reads.',
    notfresh: `The installer restarts it again; your browser asks you to choose it, twice.`,
  }[st];
  const out = pc ? 'Stop here and restart the PC without the USB stick. Nothing has changed.'
    : `Stop here and unplug ${the}. Nothing has changed; it starts its system when you turn it off and on.`;
  const B = {
    guide: [{ label: 'Restart it for me', href: go('chooser'), primary: true }, { label: 'Back', href: s.link('start') }, stop],
    hand: [{ label: 'It’s on: restart it for me', href: go('chooser'), primary: true }, { label: 'Back', href: go('guide') }, stop],
    debugging: [{ label: 'Done, back', href: go('guide'), primary: true }, stop],
    chooser: [{ label: 'Choose the device', href: go('waiting'), primary: true }, { label: 'Back', href: go('guide') }, stop],
    waiting: [{ label: 'Choose the device again', href: go('found'), primary: true }, { label: 'Back', href: go('guide') }, stop],
    found: [{ label: 'Check it', href: s.link('check'), primary: true, id: 'check' }, stop],
    notfresh: [{ label: 'Restart it for me', href: go('chooser'), primary: true }, stop],
  }[st];
  const showChip = st === 'found' || st === 'notfresh' || pc;
  const page = { ...s, m: showChip ? s.m : null };

  return html`<${Page} s=${page} step="check" eyebrow=${T[0]} title=${T[1]} lead=${T[2]}>
    ${st === 'guide' ? html`<div class="ins-card ins-card--strong">
      <span class="ins-card__title">Restart it for me</span>
      <p class="ins-small">${w.autoText || html`<${Owed}>How the installer restarts it: the description has no text for it</${Owed}>`}</p>
      <p class="ins-small ins-soft">Your browser asks twice which device to use: once for its system, once for its boot loader’s USB mode. <a class="il-link" href=${go('hand')}>USB debugging isn’t on?</a></p>
    </div>` : null}

    ${st === 'hand' || st === 'debugging' ? html`
      ${st === 'hand' && w.handText ? html`<${Admonition} label="Only through USB debugging"><p>${w.handText}</p></${Admonition}>` : null}
      <p>${w.debugText || html`<${Owed}>How to turn on USB debugging: the description has no text for it</${Owed}>`}</p>
      <p class="ins-small"><${Owed}>The menu names, step by step, on this device: the description doesn’t hold them yet</${Owed}></p>
      <${Admonition} label="Note"><p>USB debugging lets a computer you allowed control the device’s system. You can turn it off again after the installation; the installer doesn’t need it later.</p></${Admonition}>` : null}

    ${st === 'chooser' ? html`<${ChooserPicture} the=${the} name=${s.m.name} />` : null}

    ${st === 'waiting' ? html`
      <div class="ins-card ins-card--strong">
        <div class="ins-row"><span class="ins-yn ins-yn--wait" aria-hidden="true"></span><span class="ins-card__title">Waiting for ${the}</span></div>
        <p class="ins-small ins-soft">Nothing found yet. This page keeps looking.</p>
      </div>
      <section class="ins-stack">
        <h2 class="ins-h2">If nothing happens</h2>
        <${Caps}>
          ${['Is the cable in both the device and this computer?', 'Some cables only charge. Try another cable.', 'Is the device showing its boot loader’s USB mode?'].map((t) => html`<${CapRow} mark="no" glyph=${html`<${Bang} />`} name=${t} />`)}
        </${Caps}>
      </section>` : null}

    ${st === 'found' ? html`<div class="ins-card ins-card--strong">
      <div class="ins-row"><span class="ins-yn ins-yn--yes" aria-hidden="true"><${Tick} /></span><span class="ins-card__title">${pc ? 'This PC, started from the USB stick' : s.m.name}</span></div>
      <p>${pc ? 'The installer runs on this PC and reads it directly.' : `Found ${the}, in its boot loader’s USB mode.`}</p>
      <p class="ins-small ins-soft">The check starts by itself. It only reads.</p>
    </div>` : null}

    ${st === 'notfresh' ? html`<${Admonition} kind="warning" seal="G" label="Start it again" title="Not freshly started">
      <p>${The} answered, but it wasn’t started freshly into its boot loader’s USB mode. It has to be, for this installation. Nothing was changed.</p>
      <p><strong>Way out:</strong> let the installer restart it again: start its system, keep it connected, and click Restart it for me. Or stop here: it starts its system when you turn it off and on.</p>
    </${Admonition}>` : null}

    <${Actions} next=${next} out=${out} buttons=${B} />
  </${Page}>`;
}
