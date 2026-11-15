// SPDX-License-Identifier: GPL-2.0-or-later
// For the install test (web/test/live.js): the Note Air's whole install job against simulated
// devices, kept in Node so that they outlive a reload of the page, as a real device and a real
// folder do. Synthetic throughout: no boot loader, image or table of a real unit beyond the
// core's own test fixtures.
//
// World mirrors core/tests/execute.rs (its World, over core/tests/common/fbsim.rs and ramsim.rs):
// one eMMC that starts with the stock partition table of core/tests/fixtures/gpt/noteair-stock.gpt
// (vendor made 8 MiB, as there), the stock boot loader (product sdm660) and, once abl holds the
// release's boot loader, the GlassineOS one (product NoteAir, oem reboot-recovery), both with the
// Note Air's USB length bug (only equal-length transfers after the warm-up come through); the
// RAM environment of protocol version 2 when recovery holds the release's RAM environment. Every
// restart takes the device off the bus and brings it back, a moment later, as a new USB device
// of its new mode. In Android (and GlassineOS) it is not on the bus for the installer: the test
// plays the person who restarts it into its boot loader (toFastboot).
//
// The page sees it through installFakeDevices (serialised into the page): a navigator.usb whose
// devices forward every transfer to the World through window.__sim (page.exposeFunction).
// requestDevice needs the person's click (navigator.userActivation) and grants the device's USB
// ids; getDevices lists the attached device only once its ids were granted, as Chromium does for
// a device that comes back with the same ids. Tree is a folder tree the same way:
// installFakeTree's showDirectoryPicker hands out the folder the test named (tree.pick), and
// writes go through a swap, under the name only on close() (as Chromium's File System Access).
'use strict';
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');

const ROOT = path.resolve(__dirname, '..', '..');
const SECTOR = 512;
const DISK_SECTORS = 61071360;
const MIB = 1 << 20;
const CHUNK = MIB;
const MPS = 512;
const L = 65535;

const CRC = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
function crc32(b) { let c = 0xffffffff; for (const x of b) c = CRC[(c ^ x) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; }
const sha = (b) => crypto.createHash('sha256').update(b).digest();
const hex = (b) => Buffer.from(b).toString('hex');

// Deterministic bytes that differ everywhere (SHA-256 in counter mode).
function image(len, seed) {
  const out = Buffer.alloc(len);
  for (let i = 0, at = 0; at < len; i++, at += 32) sha(Buffer.from(`${seed}:${i}`)).copy(out, at, 0, Math.min(32, len - at));
  return out;
}

// The stock primary table with vendor (slot 11) at 8 MiB, as core/tests/execute.rs stock_table().
function stockTable() {
  const b = Buffer.from(fs.readFileSync(path.join(ROOT, 'core/tests/fixtures/gpt/noteair-stock.gpt')));
  const e = 1024 + 10 * 128;
  const first = b.readBigUInt64LE(e + 32);
  b.writeBigUInt64LE(first + 16383n, e + 40);
  b.writeUInt32LE(crc32(b.subarray(1024, 1024 + 48 * 128)), 512 + 88);
  b.fill(0, 512 + 16, 512 + 20);
  b.writeUInt32LE(crc32(b.subarray(512, 512 + 92)), 512 + 16);
  return b;
}

// A partition's place in a primary table: [offset, length] in bytes.
function extent(table, name) {
  const n = table.readUInt32LE(512 + 80), size = table.readUInt32LE(512 + 84), at = Number(table.readBigUInt64LE(512 + 72)) * SECTOR;
  for (let i = 0; i < n; i++) {
    const e = table.subarray(at + i * size, at + (i + 1) * size);
    if (e.length < 128) break;
    const nm = e.subarray(56, 128).toString('utf16le').replace(/\0.*$/s, '');
    if (nm === name) {
      const first = Number(e.readBigUInt64LE(32)), last = Number(e.readBigUInt64LE(40));
      return [first * SECTOR, (last - first + 1) * SECTOR];
    }
  }
  return null;
}

// A disk held sparsely: chunks of 1 MiB, zeros where never written.
class Sparse {
  constructor(size) { this.size = size; this.chunks = new Map(); }
  read(off, len) {
    const out = Buffer.alloc(len);
    for (let at = off; at < off + len;) {
      const c = Math.floor(at / CHUNK), inC = at % CHUNK, n = Math.min(CHUNK - inC, off + len - at);
      const d = this.chunks.get(c);
      if (d) d.copy(out, at - off, inC, inC + n);
      at += n;
    }
    return out;
  }
  write(off, data) {
    if (off + data.length > this.size) throw new Error('a write past the disk’s end');
    for (let at = off, end = off + data.length; at < end;) {
      const c = Math.floor(at / CHUNK), inC = at % CHUNK, n = Math.min(CHUNK - inC, end - at);
      if (!this.chunks.has(c)) this.chunks.set(c, Buffer.alloc(CHUNK));
      data.copy(this.chunks.get(c), inC, at - off, at - off + n);
      at += n;
    }
  }
}

// The Note Air's boot loaders over fastboot (fbsim.rs, Wire::NoteAir): the first transfer of a
// boot counted right, the second as 6 bytes, every later one as the one before it, a 64-byte
// command buffer that keeps what the last command left.
class FbSim {
  constructor(product, parts, vars, oemVerbs) {
    Object.assign(this, { product, parts, vars, oemVerbs, maxDownload: 595230720, commands: [], rebooted: [], gone: false, erasedWholeDisk: false });
    this.n = 0; this.prev = 0; this.cmdbuf = Buffer.alloc(64); this.expect = 0; this.download = []; this.replies = [];
  }
  count(d) { return this.n === 0 ? d.length : this.n === 1 ? 6 : this.prev; }
  command(d, count) {
    d.copy(this.cmdbuf, 0, 0, Math.min(d.length, 64));
    let cmd = this.cmdbuf.subarray(0, Math.min(count, 63));
    const z = cmd.indexOf(0);
    if (z >= 0) cmd = cmd.subarray(0, z);
    return cmd.toString('latin1');
  }
  reply(r) { this.replies.push(Buffer.from(r, 'latin1')); }
  data(d, count) {
    const got = Buffer.alloc(count, 0xee);
    d.copy(got, 0, 0, Math.min(count, d.length));
    if (got.length > this.expect) { this.expect = 0; this.download = []; return this.reply('FAILdata overrun'); }
    this.download.push(got);
    this.expect -= got.length;
    if (this.expect === 0) this.reply('OKAY');
  }
  flash(name) {
    if (name.startsWith('partition')) { for (const p of Object.values(this.parts)) p.fill(0); this.erasedWholeDisk = true; return this.reply('OKAY'); }
    const part = this.parts[name];
    if (!part) return this.reply('FAILPartition not found');
    const img = Buffer.concat(this.download);
    this.download = [];
    if (!img.length) return this.reply('FAILno data');
    if (img.length > part.length) return this.reply('FAILimage too large');
    img.copy(part, 0);
    this.reply('OKAY');
  }
  run(c) {
    this.commands.push(c);
    let m;
    if ((m = /^getvar:(.*)$/s.exec(c))) {
      const v = m[1];
      let a = null;
      if (v === 'product') a = this.product;
      else if (v === 'max-download-size') a = String(this.maxDownload);
      else if (v.startsWith('partition-size:') && this.parts[v.slice(15)]) a = '0x' + this.parts[v.slice(15)].length.toString(16);
      else if (v in this.vars) a = this.vars[v];
      return this.reply(a === null ? 'FAILunknown variable' : 'OKAY' + a);
    }
    if ((m = /^download:([0-9a-f]+)$/.exec(c))) {
      const n = parseInt(m[1], 16);
      if (!n || n > this.maxDownload) return this.reply('FAILdata too large');
      this.expect = n; this.download = [];
      return this.reply('DATA' + n.toString(16).padStart(8, '0'));
    }
    if (c.startsWith('flash:')) return this.flash(c.slice(6));
    if (c.startsWith('oem ')) {
      if (!this.oemVerbs.includes(c.slice(4))) return this.reply('FAILunknown command');
      this.reply('OKAY'); this.rebooted.push(c.slice(4)); this.gone = true; return;
    }
    if (c === 'reboot' || c === 'reboot-bootloader') { this.reply('OKAY'); this.rebooted.push(c === 'reboot' ? 'reboot' : 'bootloader'); this.gone = true; return; }
    this.reply('FAILunknown command');
  }
  send(d) {
    if (this.gone && !this.replies.length) return false;
    const count = this.count(d);
    if (this.expect > 0) this.data(d, count); else this.run(this.command(d, count));
    this.n += 1; this.prev = d.length;
    return true;
  }
  recv() { return this.replies.shift() || null; }
}

// The RAM environment of protocol version 2 (ramsim.rs RamEnv): v1 requests answered as v1, ARM
// once per start, WRITE inside one armed range, refusals with the contract's statuses.
class RamEnv {
  constructor(disks) {
    Object.assign(this, { disks, maxRead: 4 << 20, maxHash: 64 << 20, faults: {}, armed: null, armUsed: false, writes: 0, written: [], rebooted: null, gone: false });
    this.inbuf = Buffer.alloc(0); this.pending = []; this.pendingLen = 0; this.shortAt = [];
  }
  restart() { Object.assign(this, { armed: null, armUsed: false, rebooted: null, gone: false, inbuf: Buffer.alloc(0), pending: [], pendingLen: 0, shortAt: [] }); }
  out(b) {
    for (let at = 0; at < b.length; at += MIB) {
      const c = b.subarray(at, Math.min(at + MIB, b.length));
      this.pending.push(c); this.pendingLen += c.length;
      if (c.length % MPS) this.shortAt.push(this.pendingLen);
    }
  }
  respond(version, op, tag, status, offset, digest, payload = Buffer.alloc(0)) {
    const h = Buffer.alloc(64);
    h.write('GRRS', 0, 'latin1'); h.writeUInt16LE(version, 4); h.writeUInt16LE(op, 6); h.writeUInt32LE(tag, 8); h.writeUInt16LE(status, 12);
    h.writeBigUInt64LE(BigInt(payload.length), 16); h.writeBigUInt64LE(BigInt(offset), 24); digest.copy(h, 32);
    this.out(h);
    if (payload.length) this.out(payload);
  }
  feed(b) {
    this.inbuf = Buffer.concat([this.inbuf, b]);
    for (;;) {
      if (this.inbuf.length < 32) return;
      const r = this.inbuf;
      if (r.toString('latin1', 0, 4) !== 'GRRQ') throw new Error('out of step: not a request');
      const version = r.readUInt16LE(4), op = r.readUInt16LE(6), len = Number(r.readBigUInt64LE(24));
      const need = 32 + (version === 2 && op === 5 ? len : version === 2 && op === 6 ? len + 32 : 0);
      if (this.inbuf.length < need) return;
      const all = this.inbuf.subarray(0, need);
      this.inbuf = this.inbuf.subarray(need);
      this.handle(all.subarray(0, 32), all.subarray(32));
    }
  }
  handle(r, payload) {
    const version = r.readUInt16LE(4), op = r.readUInt16LE(6), tag = r.readUInt32LE(8), index = r.readUInt32LE(12);
    const off = Number(r.readBigUInt64LE(16)), len = Number(r.readBigUInt64LE(24));
    const Z = Buffer.alloc(32);
    if (version !== 1 && version !== 2) return this.respond(1, op, tag, 1, off, Z, Buffer.from('bad version'));
    const refuse = (status, why) => this.respond(version, op, tag, status, off, Z, Buffer.from(why));
    const aligned = (o, l) => o % 512 === 0 && l % 512 === 0;
    if (op === 1) {
      let t = `glassine-ramread ${version}\nmax-read ${this.maxRead}\nmax-hash ${this.maxHash}\nalign 512\n`;
      if (version === 2) t += this.armed ? `writes armed ${this.armed.length}\n` : 'writes off\n';
      this.disks.forEach(([name, d], i) => { t += `disk ${i} ${name} ${d.size} 512\n`; });
      return this.respond(version, op, tag, 0, 0, Z, Buffer.from(t));
    }
    if (op === 2 || op === 3) {
      const max = op === 2 ? this.maxRead : this.maxHash, d = this.disks[index];
      if (!d) return refuse(3, 'no such disk');
      if (!len || len > max || !aligned(off, len) || off + len > d[1].size) return refuse(4, 'out of range');
      const bytes = d[1].read(off, len);
      return this.respond(version, op, tag, 0, off, sha(bytes), op === 2 ? bytes : Buffer.alloc(0));
    }
    if (op === 5 && version === 2) {
      if (this.armUsed) return refuse(8, 'already armed in this start');
      const ranges = [];
      for (const l of payload.toString('utf8').split('\n').filter(Boolean)) {
        const [kw, disk, o, n] = l.split(' ');
        if (kw !== 'range') return refuse(1, 'parse');
        const di = /^\d+$/.test(disk) ? Number(disk) : this.disks.findIndex(([nm]) => nm === disk);
        if (!(di >= 0 && di < this.disks.length)) return refuse(3, 'no such disk');
        const [name, d] = this.disks[di], oo = Number(o), nn = Number(n);
        if (name.includes('boot') || !nn || !aligned(oo, nn) || oo + nn > d.size) return refuse(4, 'boot area, unaligned, empty or past the end');
        if (ranges.some(([x, a, b]) => x === di && oo < a + b && a < oo + nn)) return refuse(4, 'overlap');
        ranges.push([di, oo, nn]);
      }
      if (ranges.length > 64) return refuse(1, 'too many');
      this.armUsed = true; this.armed = ranges;
      const ans = Buffer.from(`armed ${ranges.length}`);
      return this.respond(version, op, tag, 0, 0, sha(ans), ans);
    }
    if (op === 6 && version === 2) {
      const data = payload.subarray(0, payload.length - 32), digest = payload.subarray(payload.length - 32);
      if (!this.armed) return refuse(6, 'not armed');
      if (index >= this.disks.length) return refuse(3, 'no such disk');
      if (!len || len > 16 << 20 || !aligned(off, len) || !this.armed.some(([x, a, b]) => x === index && off >= a && off + len <= a + b)) return refuse(4, 'not inside one armed range');
      if (!sha(data).equals(digest)) return refuse(7, 'bad data');
      const n = this.writes++;
      const d = this.disks[index][1];
      const landed = Buffer.from(data);
      if (this.faults.flipWrite === n) landed[0] ^= 0x10;
      d.write(off, landed);
      this.written.push([index, off, len]);
      const back = sha(d.read(off, len));
      return this.respond(version, op, tag, 0, off, sha(back), back);
    }
    if (op === 7 && version === 2) { this.armed = null; return this.respond(version, op, tag, 0, 0, Z); }
    if (op === 4) { this.armed = null; this.rebooted = index; this.respond(version, op, tag, 0, 0, Z); this.gone = true; return; }
    return refuse(2, 'unknown operation');
  }
  send(d) { if (this.gone) return false; this.feed(d); return true; }
  // One bulk IN of up to `max` bytes: ends at a full buffer or at the first short packet; null
  // while the device has less to send (a real transfer waits).
  recv(max) {
    const buf = Math.ceil(Math.max(max, 1) / MPS) * MPS;
    const end = this.shortAt.length && this.shortAt[0] <= buf ? this.shortAt[0] : buf;
    if (this.pendingLen < end || end === 0) return null;
    const all = Buffer.concat(this.pending);
    const out = all.subarray(0, end);
    const rest = all.subarray(end);
    this.pending = rest.length ? [rest] : []; this.pendingLen = rest.length;
    if (this.shortAt[0] === end) this.shortAt.shift();
    this.shortAt = this.shortAt.map((s) => s - end);
    return Buffer.from(out);
  }
}

// The release of core/tests/execute.rs release_files(), as files of a folder.
function releaseFiles() {
  // The layout as releases carry it (its new partitions' GUIDs empty: derived per unit).
  const recipe = JSON.parse(fs.readFileSync(path.join(ROOT, 'layouts/onyx-noteair.json'), 'utf8'));
  const files = {
    'boot-loader': [image(200 * 1024, 31), 'raw-image'],
    'ram-environment': [image(MIB, 32), 'raw-image'],
    'partition-layout': [Buffer.from(JSON.stringify(recipe, null, 2)), 'gpt-recipe'],
    kernel: [image(MIB + 300, 33), 'raw-image'],
    base: [image(3 * MIB, 34), 'btrfs-pool-image'],
    recovery: [image(MIB, 35), 'raw-image'],
  };
  const artifacts = {}, out = {};
  for (const [role, [b, kind]] of Object.entries(files)) {
    artifacts[role] = { file: `${role}.img`, sha256: hex(sha(b)), size: b.length, kind };
    out[`${role}.img`] = b;
  }
  out['release.json'] = Buffer.from(JSON.stringify({ release_format: 1, artifacts }));
  return { files: out, bytes: Object.fromEntries(Object.entries(files).map(([r, [b]]) => [r, b])) };
}

const IDS = { fastboot: [0x18d1, 0xd00d, [0xff, 0x42, 0x03]], ram: [0x1d6b, 0x0104, [0xff, 0x47, 0x52]] };

// The device, from the stock boot loader to GlassineOS. vars: the stock boot loader's answers
// (a facts snapshot's fastboot variables); faults: { flipWrite: n } (the RAM environment's n-th
// WRITE lands with a byte flipped).
class World {
  constructor({ vars, faults = {} }) {
    this.release = releaseFiles();
    this.source = stockTable();
    const disk = new Sparse(DISK_SECTORS * SECTOR);
    disk.write(0, this.source);
    const [v, n] = extent(this.source, 'vendor');
    this.vendor = image(n, 40);
    disk.write(v, this.vendor);
    this.ram = new RamEnv([['mmcblk0', disk], ['mmcblk0boot0', new Sparse(4 * MIB)]]);
    this.ram.faults = faults;
    const size = (p, d) => (vars[`partition-size:${p}`] ? parseInt(vars[`partition-size:${p}`], 16) : d);
    this.parts = { abl: image(size('abl', MIB), 41), recovery: image(size('recovery', 8 * MIB), 42), devinfo: image(size('devinfo', 4096), 43) };
    this.vars = vars;
    this.at = 'fastboot-stock';
    this.fb = null;
    this.sessions = [];
    this.granted = new Set();
    this.attached = null;
    this.ids = 0;
    this.erasedWholeDisk = false;
    this.attach();
  }
  glassine() { return this.parts.abl.subarray(0, this.release.bytes['boot-loader'].length).equals(this.release.bytes['boot-loader']); }
  // The device on the bus in its mode (a new USB device), or none.
  attach() {
    const kind = this.at.startsWith('fastboot') ? 'fastboot' : this.at === 'ram' ? 'ram' : null;
    this.fb = kind === 'fastboot'
      ? (this.glassine() ? new FbSim('NoteAir', this.parts, {}, ['reboot-recovery', 'reboot-edl']) : new FbSim('sdm660', this.parts, this.vars, []))
      : null;
    if (kind === 'ram') this.ram.restart();
    this.attached = kind ? { id: ++this.ids, kind, vid: IDS[kind][0], pid: IDS[kind][1], iface: IDS[kind][2] } : null;
  }
  // Where the device went after it restarted (execute.rs World::sync), back on the bus a moment later.
  restarted() {
    const was = this.attached;
    this.attached = null;
    if (was && was.kind === 'fastboot') {
      const s = this.fb;
      this.sessions.push(s.commands.slice());
      this.erasedWholeDisk ||= s.erasedWholeDisk;
      const r = s.rebooted[s.rebooted.length - 1];
      if (r === 'bootloader') this.at = this.glassine() ? 'fastboot' : 'fastboot-stock';
      else if (r === 'reboot-recovery') this.at = this.parts.recovery.subarray(0, MIB).equals(this.release.bytes['ram-environment']) ? 'ram' : 'android';
      else if (r === 'reboot') this.at = this.glassine() ? 'glassine' : 'android';
    } else if (was && was.kind === 'ram') {
      this.at = this.ram.rebooted === 1 ? 'fastboot' : 'glassine';
    }
    setTimeout(() => this.attach(), 300);
  }
  // The person restarts it into its boot loader (adb reboot bootloader, or the cover by hand).
  toFastboot() { this.at = this.glassine() ? 'fastboot' : 'fastboot-stock'; this.attach(); }
  // The calls of the page's fake (window.__sim).
  call(op, a) {
    const cur = this.attached;
    if (op === 'state') return { attached: cur, granted: [...this.granted] };
    if (op === 'grant') { if (cur && cur.id === a[0]) this.granted.add(`${cur.vid}:${cur.pid}`); return true; }
    if (!cur || cur.id !== a[0]) return { gone: true };
    const dev = cur.kind === 'fastboot' ? this.fb : this.ram;
    if (op === 'out') {
      const ok = dev.send(Buffer.from(a[1], 'base64'));
      if (!ok) return { gone: true };
      if (dev.gone && !this.leaving) { this.leaving = true; setTimeout(() => { this.leaving = false; this.restarted(); }, 200); }
      return { ok: true };
    }
    if (op === 'in') {
      const r = cur.kind === 'fastboot' ? dev.recv() : dev.recv(a[1]);
      if (r) return { data: r.toString('base64') };
      return dev.gone ? { gone: true } : { wait: true };
    }
    throw new Error(`no sim call ${op}`);
  }
  // The table as the disk holds it now.
  table() { return this.ram.disks[0][1].read(0, 18 * SECTOR); }
}

// A folder tree in Node: { 'Backups/x/manifest.json': Buffer, ... }; folders exist where a file
// is under them, or once made. pick: the folder the next showDirectoryPicker hands out.
class Tree {
  constructor(files = {}) { this.files = new Map(Object.entries(files)); this.dirs = new Set(); this.pick = null; for (const p of this.files.keys()) this.mkdirs(p); }
  mkdirs(p) { const parts = p.split('/'); for (let i = 1; i < parts.length; i++) this.dirs.add(parts.slice(0, i).join('/')); }
  call(op, a) {
    const [p, q] = a;
    if (op === 'pick') { const d = this.pick; return d ? { path: d, name: d.split('/').pop() } : { abort: true }; }
    if (op === 'stat') return this.files.has(p) ? { kind: 'file', size: this.files.get(p).length } : this.dirs.has(p) ? { kind: 'directory' } : null;
    if (op === 'mkdir') { this.dirs.add(p); this.mkdirs(p + '/x'); return true; }
    if (op === 'read') { const b = this.files.get(p); return b ? b.subarray(q[0], q[0] + q[1]).toString('base64') : null; }
    if (op === 'write') { this.files.set(p, Buffer.from(q, 'base64')); this.mkdirs(p); return true; }
    if (op === 'move') { if (!this.files.has(p)) return false; this.files.set(q, this.files.get(p)); this.files.delete(p); return true; }
    if (op === 'remove') return this.files.delete(p) || this.dirs.delete(p);
    if (op === 'list') { const pre = p ? p + '/' : ''; return [...new Set([...this.files.keys(), ...this.dirs].filter((x) => x.startsWith(pre)).map((x) => x.slice(pre.length).split('/')[0]))]; }
    throw new Error(`no tree call ${op}`);
  }
}

// A backup the person made earlier (emergency-read --out): its manifest and the user area, whose
// start is the stock table (the head is all an install reads of it).
function backupFiles(world, dir) {
  return {
    [`${dir}/manifest.json`]: Buffer.from(JSON.stringify({ manifest_format: 1, images: [{ name: 'emmc-user' }] })),
    [`${dir}/emmc-user.img.gz`]: zlib.gzipSync(Buffer.concat([world.source, Buffer.alloc(64 * 1024)])),
  };
}

// Serialised into the page: navigator.usb over window.__sim.
function installFakeDevices() {
  const sim = (op, ...a) => window.__sim(op, a);
  const listeners = { connect: [], disconnect: [] };
  const log = { calls: [], requested: 0 };
  window.__fakeDevices = log;
  let cur = null; // { id, desc, device }
  const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(s); };
  const unb64 = (s) => { const t = atob(s), u = new Uint8Array(t.length); for (let i = 0; i < t.length; i++) u[i] = t.charCodeAt(i); return u; };
  const le16 = (x) => [x & 0xff, x >> 8];
  const gone = () => new DOMException('The device was disconnected.', 'NotFoundError');
  const fire = (type, device) => { const ev = { type, device }; for (const f of listeners[type]) f(ev); };
  function make(desc) {
    const [c, sc, p] = desc.iface;
    const DEVICE = new Uint8Array([18, 1, ...le16(0x0200), 0, 0, 0, 64, ...le16(desc.vid), ...le16(desc.pid), ...le16(0x0100), 1, 2, 0, 1]);
    const body = [9, 4, 0, 0, 2, c, sc, p, 0, 7, 5, 0x01, 0x02, ...le16(512), 0, 7, 5, 0x81, 0x02, ...le16(512), 0];
    const CONFIG = new Uint8Array([9, 2, ...le16(9 + body.length), 1, 1, 0, 0x80, 250, ...body]);
    const view = (bytes) => new DataView(bytes.slice().buffer);
    const ep = (dir) => ({ endpointNumber: 1, direction: dir, type: 'bulk', packetSize: 512 });
    const alt = { alternateSetting: 0, interfaceClass: c, interfaceSubclass: sc, interfaceProtocol: p, interfaceName: null, endpoints: [ep('out'), ep('in')] };
    const iface = { interfaceNumber: 0, alternate: alt, alternates: [alt], claimed: false };
    const config = { configurationValue: 1, configurationName: null, interfaces: [iface] };
    const mine = () => cur && cur.id === desc.id;
    const d = {
      vendorId: desc.vid, productId: desc.pid, deviceClass: 0, deviceSubclass: 0, deviceProtocol: 0,
      deviceVersionMajor: 1, deviceVersionMinor: 0, deviceVersionSubminor: 0, usbVersionMajor: 2, usbVersionMinor: 0, usbVersionSubminor: 0,
      manufacturerName: 'Simulated', productName: desc.kind, serialNumber: null, opened: false, configuration: config, configurations: [config],
      async open() { log.calls.push('open'); if (!mine()) throw gone(); this.opened = true; },
      async close() { this.opened = false; },
      async claimInterface() { log.calls.push('claimInterface'); if (!mine()) throw gone(); iface.claimed = true; },
      async releaseInterface() { log.calls.push('releaseInterface'); if (!mine()) throw gone(); iface.claimed = false; },
      async selectConfiguration() {}, async selectAlternateInterface() {}, async reset() {}, async clearHalt() {}, async forget() {},
      async controlTransferIn(setup, length) {
        const type = setup.value >> 8;
        if (setup.requestType !== 'standard' || setup.recipient !== 'device' || setup.request !== 6 || ![1, 2].includes(type)) return { status: 'stall', data: null };
        const b = type === 1 ? DEVICE : CONFIG;
        return { status: 'ok', data: view(b.subarray(0, Math.min(length, b.length))) };
      },
      async controlTransferOut() { return { status: 'stall', bytesWritten: 0 }; },
      async transferOut(n, data) {
        const u = new Uint8Array(data.buffer ? data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) : data);
        const r = await sim('out', desc.id, b64(u));
        if (r.gone) throw gone();
        return { status: 'ok', bytesWritten: u.length };
      },
      async transferIn(n, length) {
        for (;;) {
          const r = await sim('in', desc.id, length);
          if (r.data) return { status: 'ok', data: view(unb64(r.data)) };
          if (r.gone) throw gone();
          await new Promise((res) => setTimeout(res, 15));
        }
      },
    };
    return d;
  }
  async function poll() {
    const st = await sim('state');
    const a = st.attached;
    if (cur && (!a || a.id !== cur.id)) { const was = cur; cur = null; fire('disconnect', was.device); }
    if (a && !cur) { cur = { id: a.id, desc: a, device: make(a) }; fire('connect', cur.device); }
    return st;
  }
  setInterval(() => { poll().catch(() => {}); }, 40);
  const matches = (f, desc) => (f.vendorId === undefined || f.vendorId === desc.vid) && (f.productId === undefined || f.productId === desc.pid)
    && (f.classCode === undefined || (f.classCode === desc.iface[0] && f.subclassCode === desc.iface[1] && f.protocolCode === desc.iface[2]));
  const usb = {
    async requestDevice(options) {
      log.requested += 1;
      log.calls.push('requestDevice');
      if (!navigator.userActivation.isActive) throw new DOMException('Must be handling a user gesture to show a permission request.', 'SecurityError');
      await poll();
      if (!cur || !(options.filters || []).some((f) => matches(f, cur.desc))) throw new DOMException('No device selected.', 'NotFoundError');
      await sim('grant', cur.id);
      return cur.device;
    },
    async getDevices() {
      const st = await poll();
      return cur && st.granted.includes(`${cur.desc.vid}:${cur.desc.pid}`) ? [cur.device] : [];
    },
    addEventListener(type, f) { if (listeners[type]) listeners[type].push(f); },
    removeEventListener(type, f) { if (listeners[type]) listeners[type] = listeners[type].filter((x) => x !== f); },
    onconnect: null, ondisconnect: null,
  };
  Object.defineProperty(Navigator.prototype, 'usb', { configurable: true, get: () => usb });
}

// Serialised into the page: showDirectoryPicker over window.__tree.
function installFakeTree() {
  const tree = (op, ...a) => window.__tree(op, a);
  const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(s); };
  const unb64 = (s) => { const t = atob(s), u = new Uint8Array(t.length); for (let i = 0; i < t.length; i++) u[i] = t.charCodeAt(i); return u; };
  const nf = (n) => new DOMException(`${n} not found`, 'NotFoundError');
  const bytesOf = (c) => (typeof c === 'string' ? new TextEncoder().encode(c) : c instanceof ArrayBuffer ? new Uint8Array(c) : new Uint8Array(c.buffer, c.byteOffset, c.byteLength));
  const join = (a, b) => (a ? `${a}/${b}` : b);
  // A File whose slices are read from the tree when asked (a release's files are large).
  function lazyFile(p, size, name) {
    const f = new File([], name);
    Object.defineProperty(f, 'size', { get: () => size });
    f.slice = (start = 0, end = size) => ({
      size: Math.max(0, Math.min(end, size) - start),
      async arrayBuffer() { const s = await tree('read', p, [start, Math.max(0, Math.min(end, size) - start)]); return unb64(s).buffer; },
    });
    f.arrayBuffer = () => f.slice(0, size).arrayBuffer();
    f.text = async () => new TextDecoder().decode(await f.arrayBuffer());
    f.stream = () => { let done = false; return new ReadableStream({ async pull(c) { if (done) return c.close(); done = true; c.enqueue(new Uint8Array(await f.arrayBuffer())); c.close(); } }); };
    return f;
  }
  function dir(p) {
    return {
      kind: 'directory', name: p.split('/').pop(), __path: p,
      async getDirectoryHandle(n, o = {}) {
        const s = await tree('stat', join(p, n));
        if (s && s.kind !== 'directory') throw new DOMException('a file', 'TypeMismatchError');
        if (!s) { if (!o.create) throw nf(n); await tree('mkdir', join(p, n)); }
        return dir(join(p, n));
      },
      async getFileHandle(n, o = {}) {
        const s = await tree('stat', join(p, n));
        if (s && s.kind !== 'file') throw new DOMException('a folder', 'TypeMismatchError');
        if (!s) { if (!o.create) throw nf(n); await tree('write', join(p, n), ''); }
        return file(p, n);
      },
      async removeEntry(n) { if (!(await tree('remove', join(p, n)))) throw nf(n); },
      async *keys() { yield* await tree('list', p); },
      async isSameEntry(o) { return o && o.__path === p; },
    };
  }
  function file(parent, n) {
    const p = join(parent, n);
    return {
      kind: 'file', name: n, __path: p,
      async getFile() { const s = await tree('stat', p); if (!s) throw nf(n); return lazyFile(p, s.size, n); },
      async createWritable() {
        const parts = [];
        return new WritableStream({
          write(c) { parts.push(bytesOf(c).slice()); },
          async close() { const len = parts.reduce((a, x) => a + x.length, 0), all = new Uint8Array(len); let at = 0; for (const x of parts) { all.set(x, at); at += x.length; } await tree('write', p, b64(all)); },
          abort() { parts.length = 0; },
        });
      },
      async move(to) { if (!(await tree('move', p, join(parent, to)))) throw nf(n); n = to; },
      async isSameEntry(o) { return o && o.__path === p; },
    };
  }
  window.showDirectoryPicker = async () => {
    const r = await tree('pick');
    if (r.abort) throw new DOMException('The user aborted a request.', 'AbortError');
    return dir(r.path);
  };
}

// Binds a World and a Tree to a page (before its first document).
async function bind(page, world, tree) {
  await page.exposeFunction('__sim', (op, a) => world.call(op, a));
  await page.exposeFunction('__tree', (op, a) => tree.call(op, a));
  await page.evaluateOnNewDocument(installFakeDevices);
  await page.evaluateOnNewDocument(installFakeTree);
}

module.exports = { World, Tree, Sparse, FbSim, RamEnv, bind, backupFiles, extent, stockTable, image, SECTOR, DISK_SECTORS, L };
