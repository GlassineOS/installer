// SPDX-License-Identifier: GPL-2.0-or-later
// For the live test (web/test/live.js): a simulated device in its emergency download mode (USB
// 05c6:9008) and a simulated folder the person picked, so the page runs the core's emergency read
// over WebUSB and stores into the folder unchanged. Synthetic throughout: no serial, programmer or
// disk of a real unit.
//
// installFakeEdl: the boot ROM answers Sahara as core/tests/firehose.rs's Device does (its facts
// in command mode, then the upload of the file the page sends); after DONE a programmer answers
// Firehose documents over a synthetic eMMC (8 MiB user area with a GPT, two boot areas, RPMB that
// is never to be touched). fake-usb.js's requestDevice hands it out for a 05c6:9008 filter.
// window.__fakeEdl holds what the page sent:
//   outs:       every bulk OUT: { len, sahara (the command, before DONE) | xml: { cmd, attrs } }
//   uploaded:   the bytes the boot ROM got, as hex of their sha256 (set at END_IMAGE_TX)
//   hung:       a read left unanswered (hangAtRead)
//   disk(p):    physical partition p's bytes, base64 (for the test's comparison)
// Options: refuse (the boot ROM ends the upload with an error), hangAtRead (the nth read of the
// user area gets no answer: the run is stuck mid-image, as when a tab is killed), bootSectors.
//
// installFakeDir: window.showDirectoryPicker resolves with a folder that behaves as Chromium's
// File System Access does where the test can tell: createWritable() writes to a swap and the bytes
// are under the file's name only after close() (abort drops them), getFileHandle(create) opens an
// existing file, move(name) renames (over an existing file, as Chromium does).
// window.__fakeDir: tree() -> { path: { size, open } }, bytes(path) -> base64.
//
// Both are serialised into the page (page.evaluateOnNewDocument): each uses nothing from outside
// its body.
'use strict';

function installFakeEdl({ refuse = false, hangAtRead = 0, bootSectors = 64, programmerSize, pkHash }) {
  const SECTOR = 512, SECTORS = 16384, ID = 0x0d;
  const enc = new TextEncoder(), dec = new TextDecoder();
  const le = (v, at, x, n) => { for (let i = 0; i < n; i++) { v[at + i] = Number(BigInt.asUintN(64, BigInt(x)) >> BigInt(8 * i) & 0xffn); } };
  const CRC = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
  const crc32 = (b) => { let c = 0xffffffff; for (const x of b) c = CRC[(c ^ x) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };

  // The user area: every sector a pattern of its LBA, a protective MBR, both GPTs.
  const PARTS = [['devinfo', 64, 191], ['boot_a', 256, 2303], ['userdata', 4096, 16350]];
  const disk = new Uint8Array(SECTORS * SECTOR);
  for (let lba = 0; lba < SECTORS; lba++) for (let i = 0; i < SECTOR; i++) disk[lba * SECTOR + i] = (lba * 31 + i * 7) & 0xff;
  disk.fill(0, 0, 34 * SECTOR);
  disk[446 + 4] = 0xee; disk[510] = 0x55; disk[511] = 0xaa;
  const entries = new Uint8Array(128 * 128);
  PARTS.forEach(([name, first, last], i) => {
    const e = entries.subarray(i * 128, (i + 1) * 128);
    e[0] = 0xa0 + i; e[16] = 0x10 + i;
    le(e, 32, first, 8); le(e, 40, last, 8);
    [...name].forEach((ch, k) => le(e, 56 + 2 * k, ch.charCodeAt(0), 2));
  });
  const h = new Uint8Array(92);
  h.set(enc.encode('EFI PART'));
  le(h, 8, 0x00010000, 4); le(h, 12, 92, 4); le(h, 24, 1, 8); le(h, 32, SECTORS - 1, 8);
  le(h, 40, 34, 8); le(h, 48, SECTORS - 34, 8); h[56] = 0x77; le(h, 72, 2, 8);
  le(h, 80, 128, 4); le(h, 84, 128, 4); le(h, 88, crc32(entries), 4);
  le(h, 16, crc32(h), 4);
  disk.set(h, SECTOR); disk.set(entries, 2 * SECTOR);
  const b = h.slice();
  le(b, 16, 0, 4); le(b, 24, SECTORS - 1, 8); le(b, 32, 1, 8); le(b, 72, SECTORS - 33, 8);
  le(b, 16, crc32(b), 4);
  const end = SECTORS * SECTOR;
  disk.set(entries, end - 33 * SECTOR);
  disk.fill(0, end - SECTOR, end);
  disk.set(b, end - SECTOR);
  const boot = [1, 2].map((p) => { const a = new Uint8Array(bootSectors * SECTOR); for (let i = 0; i < a.length; i++) a[i] = ((i / SECTOR) | 0) ^ ((i * (p * 3 + 1)) & 0xff); return a; });
  const rpmb = new Uint8Array(64 * SECTOR).fill(0x5a);
  const areas = [disk, boot[0], boot[1], rpmb];

  const pkt = (c, body) => { const v = new Uint8Array(8 + 4 * body.length); le(v, 0, c, 4); le(v, 4, v.length, 4); body.forEach((w, i) => le(v, 8 + 4 * i, w, 4)); return v; };
  const hash = pkHash ? Uint8Array.from(pkHash.match(/../g).map((x) => parseInt(x, 16))) : new Uint8Array(32);
  const u64 = (x) => { const v = new Uint8Array(8); le(v, 0, x, 8); return v; };
  const out = [
    pkt(1, [2, 1, 0x400, 3, 0, 0, 0, 0, 0, 0]),
    pkt(0x0b, []),
    pkt(0x0e, [1, 4]), Uint8Array.of(4, 3, 2, 1),
    pkt(0x0e, [2, 8]), u64(0x000cc0e100000000n),
    pkt(0x0e, [3, 32]), hash,
    pkt(0x0e, [7, 4]), Uint8Array.of(3, 0, 0, 0),
    pkt(1, [2, 1, 0x400, 0, 0, 0, 0, 0, 0, 0]),
    pkt(0x03, [ID, 0, programmerSize]),
  ];
  const log = { outs: [], uploaded: null, hung: false, gone: false, calls: [] };
  window.__fakeEdl = log;
  let up = [], upLen = 0, done = false, reads = 0;
  const waiting = [];
  const doc = (body) => enc.encode(`<?xml version="1.0" encoding="UTF-8" ?>\n<data>\n${body}\n</data>`);
  const logDoc = (v) => doc(`<log value="${v}" />`);
  let device = null;
  const unplug = () => {
    if (log.gone) return;
    log.gone = true;
    for (const f of waiting.splice(0)) f(new DOMException('The device was disconnected.', 'NotFoundError'));
    const ev = { type: 'disconnect', device };
    for (const f of window.__fakeUsbDisconnect || []) f(ev);
  };
  const b64 = (u) => { let s = ''; for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode(...u.subarray(i, i + 0x8000)); return btoa(s); };
  log.disk = (p) => b64(areas[p]);

  async function sahara(d) {
    const c = d.length >= 8 ? new DataView(d.buffer, d.byteOffset).getUint32(0, true) : null;
    // After the second HELLO_RESP (the one for the image transfer), the programmer's bytes.
    if (upLen < programmerSize && log.outs.filter((o) => o.sahara === 2).length >= 2) {
      up.push(d.slice()); upLen += d.length;
      log.outs.push({ len: d.length, sahara: 'data' });
      if (upLen >= programmerSize) {
        const all = new Uint8Array(upLen); let at = 0; for (const x of up) { all.set(x, at); at += x.length; }
        log.uploaded = [...new Uint8Array(await crypto.subtle.digest('SHA-256', all))].map((x) => x.toString(16).padStart(2, '0')).join('');
        if (refuse) out.push(pkt(0x04, [ID, 0x25]));
        else out.push(pkt(0x04, [ID, 0]));
      }
      return;
    }
    log.outs.push({ len: d.length, sahara: c });
    if (c === 5) { done = true; out.push(pkt(0x06, [1]), logDoc('INFO: Binary build date: synthetic'), logDoc('INFO: synthetic programmer')); }
    if (c === 7) { out.push(pkt(0x08, [])); setTimeout(unplug, 300); }
  }

  function firehose(d) {
    const text = dec.decode(d);
    const m = /<data>\s*<([A-Za-z_]+)/.exec(text);
    const attrs = Object.fromEntries([...text.matchAll(/([A-Za-z_]+)="([^"]*)"/g)].map((x) => [x[1], x[2]]));
    const cmd = m ? m[1] : null;
    log.outs.push({ len: d.length, xml: { cmd, attrs } });
    const p = Number(attrs.physical_partition_number);
    if (cmd === 'configure') {
      const n = attrs.MaxPayloadSizeToTargetInBytes;
      out.push(doc(`<response value="ACK" MinVersionSupported="1" MemoryName="eMMC" MaxPayloadSizeFromTargetInBytes="4096" MaxPayloadSizeToTargetInBytes="${n}" MaxPayloadSizeToTargetInBytesSupported="${n}" MaxXMLSizeInBytes="4096" Version="1" TargetName="synthetic" />`));
    } else if (cmd === 'getstorageinfo') {
      const total = areas[p] ? areas[p].length / SECTOR : 0;
      const name = p === 3 ? ', "phys_partition_name":"RPMB"' : '';
      out.push(logDoc(`INFO: {"storage_info": {"total_blocks":${total}, "block_size":512, "page_size":512, "num_physical":4, "fw_version":"1", "mem_type":"eMMC", "prod_name":"SYNTH"${name}}}`), doc('<response value="ACK" />'));
    } else if (cmd === 'read') {
      const start = Number(attrs.start_sector), n = Number(attrs.num_partition_sectors), area = areas[p];
      if (!area || start + n > area.length / SECTOR) { out.push(logDoc('ERROR: sectors out of range'), doc('<response value="NAK" />')); return; }
      if (p === 0 && ++reads === hangAtRead) { log.hung = true; return; }
      out.push(doc('<response value="ACK" rawmode="true" />'), area.slice(start * SECTOR, (start + n) * SECTOR), doc('<response value="ACK" rawmode="false" />'));
    } else if (cmd === 'power') {
      out.push(doc('<response value="ACK" />'));
      setTimeout(unplug, 300);
    } else {
      out.push(doc('<response value="NAK" />'));
    }
  }

  const DEVICE = new Uint8Array([18, 1, 0x00, 0x02, 0, 0, 0, 64, 0xc6, 0x05, 0x08, 0x90, 0x00, 0x01, 1, 2, 0, 1]);
  const body = [9, 4, 0, 0, 2, 0xff, 0xff, 0xff, 0, 7, 5, 0x01, 0x02, 0x00, 0x02, 0, 7, 5, 0x81, 0x02, 0x00, 0x02, 0];
  const CONFIG = new Uint8Array([9, 2, 9 + body.length, 0, 1, 1, 0, 0x80, 250, ...body]);
  const view = (bytes) => new DataView(bytes.slice().buffer);
  const ep = (dir) => ({ endpointNumber: 1, direction: dir, type: 'bulk', packetSize: 512 });
  const alt = { alternateSetting: 0, interfaceClass: 0xff, interfaceSubclass: 0xff, interfaceProtocol: 0xff, interfaceName: null, endpoints: [ep('out'), ep('in')] };
  const iface = { interfaceNumber: 0, alternate: alt, alternates: [alt], claimed: false };
  const config = { configurationValue: 1, configurationName: null, interfaces: [iface] };
  const called = (name) => log.calls.push(name);
  const gone = () => Promise.reject(new DOMException('The device was disconnected.', 'NotFoundError'));
  device = {
    vendorId: 0x05c6, productId: 0x9008, deviceClass: 0, deviceSubclass: 0, deviceProtocol: 0,
    deviceVersionMajor: 1, deviceVersionMinor: 0, deviceVersionSubminor: 0,
    usbVersionMajor: 2, usbVersionMinor: 0, usbVersionSubminor: 0,
    manufacturerName: 'Qualcomm CDMA Technologies MSM', productName: 'QUSB__BULK', serialNumber: null,
    opened: false, configuration: config, configurations: [config],
    async open() { called('open'); this.opened = true; },
    async close() { called('close'); this.opened = false; },
    async claimInterface() { called('claimInterface'); iface.claimed = true; },
    async releaseInterface() {
      called('releaseInterface');
      for (const f of waiting.splice(0)) f(new DOMException('The transfer was cancelled.', 'AbortError'));
      if (log.gone) return gone();
      iface.claimed = false;
    },
    async selectConfiguration() { called('selectConfiguration'); },
    async selectAlternateInterface() { called('selectAlternateInterface'); },
    async reset() { called('reset'); },
    async clearHalt() { called('clearHalt'); },
    async forget() { called('forget'); },
    async controlTransferIn(setup, length) {
      called('controlTransferIn');
      const type = setup.value >> 8;
      if (setup.requestType !== 'standard' || setup.request !== 6 || ![1, 2].includes(type)) return { status: 'stall', data: null };
      const d = type === 1 ? DEVICE : CONFIG;
      return { status: 'ok', data: view(d.subarray(0, Math.min(length, d.length))) };
    },
    async controlTransferOut() { called('controlTransferOut'); return { status: 'stall', bytesWritten: 0 }; },
    async transferOut(n, data) {
      if (log.gone) return gone();
      const d = new Uint8Array(data.buffer ? data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) : data);
      if (done) firehose(d); else await sahara(d);
      return { status: 'ok', bytesWritten: d.length };
    },
    async transferIn(n, length) {
      if (log.gone) return gone();
      let r = out.shift();
      if (!r) return new Promise((_, fail) => waiting.push(fail));
      if (r.length > length) { out.unshift(r.subarray(length)); r = r.subarray(0, length); }
      return { status: 'ok', data: view(r) };
    },
  };
  window.__fakeEdlDevice = device;
}

function installFakeDir({ name = 'Backups' } = {}) {
  const notFound = (n) => new DOMException(`${n} not found`, 'NotFoundError');
  const concat = (parts) => { const n = parts.reduce((a, x) => a + x.length, 0), o = new Uint8Array(n); let at = 0; for (const x of parts) { o.set(x, at); at += x.length; } return o; };
  const bytesOf = (c) => (typeof c === 'string' ? new TextEncoder().encode(c) : c instanceof ArrayBuffer ? new Uint8Array(c.slice(0)) : new Uint8Array(c.buffer.slice(c.byteOffset, c.byteOffset + c.byteLength)));
  function dirHandle(node) {
    return {
      kind: 'directory',
      get name() { return node.name; },
      async getDirectoryHandle(n, opts = {}) {
        const e = node.entries.get(n);
        if (e) { if (e.kind !== 'directory') throw new DOMException('a file', 'TypeMismatchError'); return dirHandle(e); }
        if (!opts.create) throw notFound(n);
        const d = { kind: 'directory', name: n, entries: new Map() };
        node.entries.set(n, d);
        return dirHandle(d);
      },
      async getFileHandle(n, opts = {}) {
        const e = node.entries.get(n);
        if (e) { if (e.kind !== 'file') throw new DOMException('a folder', 'TypeMismatchError'); return fileHandle(node, e); }
        if (!opts.create) throw notFound(n);
        const f = { kind: 'file', name: n, data: new Uint8Array(0), open: 0 };
        node.entries.set(n, f);
        return fileHandle(node, f);
      },
      async removeEntry(n) { if (!node.entries.delete(n)) throw notFound(n); },
      async *keys() { yield* node.entries.keys(); },
    };
  }
  function fileHandle(parent, f) {
    return {
      kind: 'file',
      get name() { return f.name; },
      async getFile() { if (parent.entries.get(f.name) !== f) throw notFound(f.name); return new File([f.data], f.name); },
      async createWritable() {
        const swap = [];
        f.open++;
        return new WritableStream({
          write(c) { swap.push(bytesOf(c)); },
          close() { f.data = concat(swap); f.open--; },
          abort() { f.open--; },
        });
      },
      async move(to) {
        if (typeof to !== 'string') throw new TypeError('only a rename in the same folder is simulated');
        if (parent.entries.get(f.name) !== f) throw notFound(f.name);
        parent.entries.delete(f.name);
        f.name = to;
        parent.entries.set(to, f);
      },
    };
  }
  const root = { kind: 'directory', name, entries: new Map() };
  const tree = (node, at, o) => {
    for (const [n, e] of node.entries) {
      const p = at ? `${at}/${n}` : n;
      if (e.kind === 'directory') { o[p + '/'] = { dir: true }; tree(e, p, o); } else o[p] = { size: e.data.length, open: e.open };
    }
    return o;
  };
  const find = (path) => path.split('/').reduce((n, part) => n && n.entries && n.entries.get(part), root);
  const b64 = (u) => { let s = ''; for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode(...u.subarray(i, i + 0x8000)); return btoa(s); };
  window.__fakeDir = { picked: 0, tree: () => tree(root, '', {}), bytes: (p) => b64(find(p).data) };
  window.showDirectoryPicker = async (opts) => { window.__fakeDir.picked++; window.__fakeDir.opts = opts; return dirHandle(root); };
}

module.exports = { installFakeEdl, installFakeDir };
