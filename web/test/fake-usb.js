// SPDX-License-Identifier: GPL-2.0-or-later
// A simulated fastboot device behind a fake navigator.usb, for the live test (web/test/live.js).
// The page runs the core's WebUSB code against it unchanged: nusb reads the device and
// configuration descriptors (GET_DESCRIPTOR), claims the fastboot interface and does bulk
// transfers. The device answers getvar from a facts snapshot and has the Note Air's USB driver
// bug (GlassineOS/abl#1), as the Sim of core/tests/inspect.rs: the first transfer of a boot is
// counted right, the second as 6 bytes, every later one as the length of the one before it, and
// its 64-byte command buffer keeps what the last command left in it. So only the equal-length
// framing, with its warm-up, gets the right answers.
//
// installFakeUsb is serialised into the page (page.evaluateOnNewDocument): it uses nothing from
// outside its body. window.__fakeUsb holds what the page sent:
//   outs:   every bulk OUT transfer: { len, cmd (up to the first NUL), nulPadded, allX, seen }
//           (seen: the command the simulated device decoded from it, under its length bug)
//   calls:  every other call on the device or navigator.usb, by name, with its arguments
//   unplug(): the device goes away: a disconnect event, and every later call on it fails
// A device that answered `reboot` goes away the same way shortly after (with silentReboot it
// goes without answering, as a boot loader may). A transfer still waiting fails when it goes. Every OUT transfer is
// also appended, as it is made, to sessionStorage['fakeUsb.outs'], which outlives a reload of
// the page (the pagehide test reads it there).
'use strict';

function installFakeUsb({ vars, warm = false, cancel = false, silentReboot = false, serial = 'SECRET-UNIT-1234' }) {
  const log = { outs: [], calls: [], requested: 0, gone: false };
  window.__fakeUsb = log;
  const keep = (o) => {
    try {
      const k = 'fakeUsb.outs', all = JSON.parse(sessionStorage.getItem(k) || '[]');
      all.push(o); sessionStorage.setItem(k, JSON.stringify(all));
    } catch { /* no storage in this document */ }
  };
  const listeners = { connect: [], disconnect: [] };
  const waiting = [];
  const enc = new TextEncoder(), dec = new TextDecoder();
  const sim = { n: warm ? 5 : 0, prev: warm ? 30 : 0, cmdbuf: new Uint8Array(64), replies: [] };
  if (warm) sim.cmdbuf.set(enc.encode('getvar:max-download-size'));

  function command(d) {
    const count = sim.n === 0 ? d.length : sim.n === 1 ? 6 : sim.prev;
    sim.cmdbuf.set(d.subarray(0, Math.min(d.length, 64)));
    let cmd = sim.cmdbuf.slice(0, Math.min(count, 63));
    const z = cmd.indexOf(0);
    if (z >= 0) cmd = cmd.slice(0, z);
    return dec.decode(cmd);
  }
  function answer(c) {
    const v = c.startsWith('getvar:') ? c.slice(7) : null;
    if (v !== null) return v in vars ? `OKAY${vars[v]}` : 'FAILunknown variable';
    if (c === 'reboot') return 'OKAY';
    return 'FAILunknown command';
  }

  const le16 = (x) => [x & 0xff, x >> 8];
  const DEVICE = new Uint8Array([18, 1, ...le16(0x0200), 0, 0, 0, 64, ...le16(0x18d1), ...le16(0xd00d), ...le16(0x0100), 1, 2, 3, 1]);
  const body = [
    9, 4, 0, 0, 2, 0xff, 0x42, 0x03, 0, // interface 0: fastboot
    7, 5, 0x01, 0x02, ...le16(512), 0, // bulk OUT 1
    7, 5, 0x81, 0x02, ...le16(512), 0, // bulk IN 1
  ];
  const CONFIG = new Uint8Array([9, 2, ...le16(9 + body.length), 1, 1, 0, 0x80, 250, ...body]);
  // A DataView over an ArrayBuffer of exactly these bytes (nusb copies the whole buffer).
  const view = (bytes) => new DataView(bytes.slice().buffer);

  const ep = (dir) => ({ endpointNumber: 1, direction: dir, type: 'bulk', packetSize: 512 });
  const alt = { alternateSetting: 0, interfaceClass: 0xff, interfaceSubclass: 0x42, interfaceProtocol: 0x03, interfaceName: null, endpoints: [ep('out'), ep('in')] };
  const iface = { interfaceNumber: 0, alternate: alt, alternates: [alt], claimed: false };
  const config = { configurationValue: 1, configurationName: null, interfaces: [iface] };
  const called = (name, args) => { log.calls.push({ name, args: JSON.parse(JSON.stringify(args)) }); };
  const never = () => new Promise(() => {});
  const gone = () => Promise.reject(new DOMException('The device was disconnected.', 'NotFoundError'));
  const unplug = () => {
    if (log.gone) return;
    log.gone = true; iface.claimed = false; device.opened = false;
    for (const fail of waiting.splice(0)) fail(new DOMException('The device was disconnected.', 'NotFoundError'));
    const ev = { type: 'disconnect', device };
    for (const f of listeners.disconnect) f(ev);
    if (typeof usb.ondisconnect === 'function') usb.ondisconnect(ev);
  };
  log.unplug = unplug;

  const device = {
    vendorId: 0x18d1, productId: 0xd00d, deviceClass: 0, deviceSubclass: 0, deviceProtocol: 0,
    deviceVersionMajor: 1, deviceVersionMinor: 0, deviceVersionSubminor: 0,
    usbVersionMajor: 2, usbVersionMinor: 0, usbVersionSubminor: 0,
    manufacturerName: 'Google', productName: 'Android', serialNumber: serial,
    opened: false, configuration: config, configurations: [config],
    async open() { called('open', []); this.opened = true; },
    async close() { called('close', []); this.opened = false; },
    async claimInterface(n) { called('claimInterface', [n]); iface.claimed = true; },
    async releaseInterface(n) { called('releaseInterface', [n]); if (log.gone) return gone(); iface.claimed = false; },
    async selectConfiguration(v) { called('selectConfiguration', [v]); },
    async selectAlternateInterface(n, a) { called('selectAlternateInterface', [n, a]); },
    async reset() { called('reset', []); },
    async clearHalt(d, e) { called('clearHalt', [d, e]); },
    async forget() { called('forget', []); },
    async controlTransferIn(setup, length) {
      called('controlTransferIn', [setup, length]);
      const type = setup.value >> 8;
      if (setup.requestType !== 'standard' || setup.recipient !== 'device' || setup.request !== 6 || ![1, 2].includes(type)) {
        return { status: 'stall', data: null };
      }
      const d = type === 1 ? DEVICE : CONFIG;
      return { status: 'ok', data: view(d.subarray(0, Math.min(length, d.length))) };
    },
    async controlTransferOut(setup, data) { called('controlTransferOut', [setup, data ? data.byteLength : 0]); return { status: 'stall', bytesWritten: 0 }; },
    async isochronousTransferIn(...a) { called('isochronousTransferIn', a); return never(); },
    async isochronousTransferOut(...a) { called('isochronousTransferOut', a); return never(); },
    async transferOut(n, data) {
      if (log.gone) { called('transferOut-unplugged', [n]); return gone(); }
      const d = new Uint8Array(data.buffer ? data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) : data);
      const z = d.indexOf(0), end = z < 0 ? d.length : z;
      const c = command(d);
      const o = { endpoint: n, len: d.length, cmd: dec.decode(d.subarray(0, Math.min(end, 64))), nulPadded: d.subarray(end).every((b) => b === 0), allX: d.every((b) => b === 0x78), seen: c };
      log.outs.push(o); keep(o);
      sim.n += 1; sim.prev = d.length;
      // It restarts: off the bus a moment after its answer (or, silently, without one).
      if (c === 'reboot' && silentReboot) setTimeout(unplug, 300);
      else sim.replies.push(enc.encode(answer(c)));
      if (c === 'reboot' && !silentReboot) setTimeout(unplug, 300);
      return { status: 'ok', bytesWritten: d.length };
    },
    async transferIn(n, length) {
      if (log.gone) return gone();
      const r = sim.replies.shift();
      if (!r) { called('transferIn-unanswered', [n, length]); return new Promise((_, fail) => waiting.push(fail)); }
      return { status: 'ok', data: view(r) };
    },
  };

  const usb = {
    async requestDevice(options) {
      called('requestDevice', [options]); log.requested += 1;
      if (cancel) throw new DOMException('No device selected.', 'NotFoundError');
      return device;
    },
    async getDevices() { called('getDevices', []); return []; },
    addEventListener(type, f) { if (listeners[type]) listeners[type].push(f); },
    removeEventListener(type, f) { if (listeners[type]) listeners[type] = listeners[type].filter((x) => x !== f); },
    onconnect: null, ondisconnect: null,
  };
  Object.defineProperty(Navigator.prototype, 'usb', { configurable: true, get: () => usb });
}

module.exports = { installFakeUsb };
