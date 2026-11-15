// SPDX-License-Identifier: GPL-2.0-or-later
// The page's side of an installation (wasm install_run, its InstallHost and ReleaseFiles): the
// release's files, the record and a step's originals in the backup folder, the backup's bytes,
// and reaching the device over WebUSB. The core carries out the plan; this file only moves bytes
// and asks the person.
//
// Files, as the command line's (cli/src/install.rs), all in the folder of the backup the plan
// rests on (the person picked it, or the backup of this page made it):
//
//   install-record.json           the record (spec 001 FR-014): each save a whole copy, written
//                                 first as install-record.json.partial, then put under its name
//   install-record.kept/NAME      what a step read before it first wrote (a boot image)
//   install-record.inputs.json    the person's answers the record doesn't hold: { developer }
//                                 (untried write steps run only for a developer who asked)
//   manifest.json, emmc-user.img.gz
//                                 the backup itself: only read
//
// A browser writes a file through a swap file of its own and puts the bytes under the file's
// name only when it is closed, so a page closed in the middle of a save leaves the record of
// the save before it.
//
// The release is a folder the person picked: its release.json and the files it names, read in
// slices when the core asks (nothing is read whole, nothing is uploaded or kept).

export const RECORD = 'install-record.json';
const PARTIAL = `${RECORD}.partial`;
const KEPT = `${RECORD}.kept`;
const INPUTS = 'install-record.inputs.json';
const USER_AREA = 'emmc-user.img.gz';
const MANIFEST = 'manifest.json';
export const RELEASE_MANIFEST = 'release.json';

async function file(dir, name) {
  try {
    return await (await dir.getFileHandle(name)).getFile();
  } catch (e) {
    if (e && (e.name === 'NotFoundError' || e.name === 'TypeMismatchError')) return null;
    throw e;
  }
}

async function bytesOf(dir, name) {
  const f = await file(dir, name);
  return f ? new Uint8Array(await f.arrayBuffer()) : null;
}

// Writes `bytes` under `name` whole (the browser's swap file makes it all or nothing).
async function writeWhole(dir, name, bytes) {
  const h = await dir.getFileHandle(name, { create: true });
  const w = (await h.createWritable({ keepExistingData: false })).getWriter();
  try {
    await w.write(bytes);
    await w.close();
  } catch (e) {
    try { await w.abort(); } catch { /* gone already */ }
    throw e;
  }
}

// ---- the release ------------------------------------------------------------------------------

// The release in folder `dir`: its manifest and its files (wasm ReleaseFiles). Paths in the
// manifest are relative and plain (the core checks them); a/b.img is read from folder a.
export async function openRelease(dir) {
  const m = await file(dir, RELEASE_MANIFEST);
  if (!m) throw new Error(`${dir.name} has no ${RELEASE_MANIFEST}: choose the folder of a GlassineOS release`);
  const manifest = await m.text();
  let parsed;
  try { parsed = JSON.parse(manifest); } catch (e) { throw new Error(`${dir.name}/${RELEASE_MANIFEST} isn’t a release manifest: ${e.message}`); }
  if (!parsed || typeof parsed.artifacts !== 'object') throw new Error(`${dir.name}/${RELEASE_MANIFEST} names no artifacts`);
  const open = new Map();
  const get = async (path) => {
    if (!open.has(path)) {
      open.set(path, (async () => {
        const parts = path.split('/');
        let d = dir;
        for (const p of parts.slice(0, -1)) d = await d.getDirectoryHandle(p);
        const f = await file(d, parts[parts.length - 1]);
        if (!f) throw new Error(`${path}: not in the release folder`);
        return f;
      })());
      open.get(path).catch(() => open.delete(path));
    }
    return open.get(path);
  };
  return {
    name: dir.name,
    manifest,
    artifacts: parsed.artifacts,
    files: {
      origin: () => `the release folder ${dir.name}`,
      size: async (path) => (await get(path)).size,
      read: async (path, offset, len) => new Uint8Array(await (await get(path)).slice(offset, offset + len).arrayBuffer()),
    },
  };
}

// ---- the backup folder: the backup, the record, the originals ----------------------------------

// What the folder holds: a finished backup (its manifest and the whole user area) and a record.
export async function folderState(dir) {
  return {
    manifest: !!(await file(dir, MANIFEST)),
    userArea: !!(await file(dir, USER_AREA)),
    record: !!(await file(dir, RECORD)) || !!(await file(dir, PARTIAL)),
  };
}

// The record as last saved: under its name, or, when a save was cut between its two writes,
// the whole copy before it went under the name.
export async function readRecord(dir) {
  const b = (await bytesOf(dir, RECORD)) || (await bytesOf(dir, PARTIAL));
  return b ? new TextDecoder().decode(b) : null;
}

// The answers kept beside the record, or null.
export async function readInputs(dir) {
  const b = await bytesOf(dir, INPUTS);
  if (!b) return null;
  try { return JSON.parse(new TextDecoder().decode(b)); } catch { return null; }
}

// The record's store (wasm AsyncRecordStore): write_temp, commit, read.
function recordStore(dir) {
  let temp = null;
  return {
    async write_temp(bytes) {
      temp = bytes.slice();
      await writeWhole(dir, PARTIAL, temp);
    },
    async commit() {
      if (!temp) throw new Error('nothing to put under the record’s name');
      await writeWhole(dir, RECORD, temp);
      temp = null;
      try { await dir.removeEntry(PARTIAL); } catch { /* the copy under the name counts */ }
    },
    async read() {
      return bytesOf(dir, RECORD);
    },
  };
}

// A new record: refused when the folder holds one (that installation is resumed, not replaced).
export async function saveNewRecord(dir, text, inputs) {
  if ((await folderState(dir)).record) throw new Error(`${dir.name} holds an installation record already: resume that installation instead`);
  try { await dir.removeEntry(KEPT, { recursive: true }); } catch { /* none */ }
  await writeWhole(dir, INPUTS, new TextEncoder().encode(JSON.stringify(inputs) + '\n'));
  const st = recordStore(dir);
  await st.write_temp(new TextEncoder().encode(text));
  await st.commit();
}

// The backed-up user area's bytes from `offset` (gzip): read from the start each time, with the
// largest head read so far kept (the core reads the partition table, at the disk's start).
const HEAD_KEEP = 64 << 20;
function backupReader(dir) {
  let head = new Uint8Array(0);
  return async (offset, len) => {
    const end = offset + len;
    if (end > head.length) {
      const f = await file(dir, USER_AREA);
      if (!f) throw new Error(`${dir.name} has no ${USER_AREA}`);
      const r = f.stream().pipeThrough(new DecompressionStream('gzip')).getReader();
      // Kept: the head (up to HEAD_KEEP), or only [offset, end) for a read further in.
      const keepFrom = end <= HEAD_KEEP ? 0 : offset;
      const chunks = [];
      let pos = 0;
      while (pos < end) {
        const { done, value } = await r.read();
        if (done) break;
        const a = Math.max(keepFrom - pos, 0), b = Math.min(end - pos, value.length);
        if (b > a) chunks.push(value.subarray(a, b));
        pos += value.length;
      }
      r.cancel().catch(() => {});
      if (pos < end) throw new Error(`the backup ends at ${pos} bytes, before ${end}`);
      const all = new Uint8Array(end - keepFrom);
      let at = 0;
      for (const c of chunks) { all.set(c, at); at += c.length; }
      if (keepFrom === 0) head = all;
      return all.slice(offset - keepFrom, end - keepFrom);
    }
    return head.slice(offset, end);
  };
}

// ---- reaching the device -----------------------------------------------------------------------

const hex = (s) => parseInt(s, 16);
// The WebUSB filter of a description mode: its USB ids, and its interface where it has one.
function filterOf(mode) {
  const u = mode.usb || {};
  const f = { vendorId: hex(u.vendor_id), productId: hex(u.product_id) };
  if (u.interface) {
    const [c, sc, p] = u.interface.split('-').map(hex);
    Object.assign(f, { classCode: c, subclassCode: sc, protocolCode: p });
  }
  return f;
}
const matches = (d, f) => d.vendorId === f.vendorId && d.productId === f.productId;
// The link the core takes for a mode's kind; the other kinds the browser can't talk to.
const LINKS = { fastboot: 'fastboot', 'glassine-ram': 'ram' };

// The host (wasm InstallHost) of one run. dir: the backup folder; modes: the description's modes
// ({id, kind}); on(event): what happens, for the screen:
//   { kind: 'saved', record } · { kind: 'progress', step, done, total } · { kind: 'note', line }
//   { kind: 'prompt', prompt } (null when it is answered)
// A prompt: { mode, how: {kind, transition, text}, link, elsewhere: [{id, kind}], choose(),
// elsewhereIs(id), stop(), error } — choose() must be called from the person's click (the
// browser's chooser needs one).
export function installHost({ dir, modes, on, cancelled, usb = navigator.usb }) {
  const store = recordStore(dir);
  const backup = backupReader(dir);
  let last = null; // the device handed to the core last
  let pending = null;
  return {
    write_temp: (b) => store.write_temp(b),
    async commit() {
      await store.commit();
      const text = await readRecord(dir);
      on({ kind: 'saved', record: text });
    },
    read: () => store.read(),
    async back_up() {
      const st = await folderState(dir);
      return { location: dir.name, manifest: await bytesOf(dir, MANIFEST), user_area: st.userArea };
    },
    backup_bytes: (offset, len) => backup(offset, len),
    async keep(name, bytes) {
      const k = await dir.getDirectoryHandle(KEPT, { create: true });
      await writeWhole(k, name, bytes.slice());
    },
    async kept(name) {
      let k;
      try { k = await dir.getDirectoryHandle(KEPT); } catch (e) { if (e && e.name === 'NotFoundError') return null; throw e; }
      return bytesOf(k, name);
    },
    progress: (step, done, total) => on({ kind: 'progress', step, done, total }),
    note: (line) => on({ kind: 'note', line }),
    cancel_requested: () => cancelled(),
    now: () => new Date().toISOString().replace(/\.\d+Z$/, 'Z'),

    // Waits for the device in `mode`: a device the browser already lets this page use is taken
    // at once (after a restart: only a new one, the old one went away); otherwise the person is
    // asked, and the browser's chooser opens on their click.
    reach(modeJson, howJson) {
      const mode = JSON.parse(modeJson), how = JSON.parse(howJson);
      const link = LINKS[mode.kind] || null;
      const filter = link ? filterOf(mode) : null;
      const restart = how.kind !== 'already';
      const elsewhere = modes.filter((m) => !LINKS[m.kind] && ['android-adb', 'glassine-usb'].includes(m.kind) && (how.kind === 'already' || m.id === mode.id));
      return new Promise((resolve, reject) => {
        let done = false, timer = null;
        const finish = (f, v) => {
          if (done) return;
          done = true;
          clearInterval(timer);
          if (pending === prompt) pending = null;
          on({ kind: 'prompt', prompt: null });
          f(v);
        };
        const take = (device) => { last = device; finish(resolve, { link, device }); };
        const prompt = {
          mode, how, link, elsewhere,
          // The browser's chooser: only this mode's device.
          choose() {
            if (!link) return Promise.resolve();
            prompt.error = null;
            const asked = usb.requestDevice({ filters: [filter] });
            return asked.then((d) => { if (!done) take(d); }, (e) => {
              if (e && e.name === 'NotFoundError') return; // the chooser was closed
              prompt.error = e && e.message ? e.message : String(e);
              on({ kind: 'prompt', prompt });
            });
          },
          elsewhereIs(id) { finish(resolve, { link: 'elsewhere', mode: id }); },
          stop() { finish(reject, new Error('stopped by you while the installer waited for the device')); },
          error: null,
        };
        pending = prompt;
        // A device the page may use already, found by looking now and then (connect events
        // aren't sent for a device the page isn't allowed yet).
        const look = async () => {
          if (done || !link) return;
          let ds;
          try { ds = await usb.getDevices(); } catch { return; }
          const d = ds.find((x) => matches(x, filter) && !(restart && x === last));
          if (d && !done) take(d);
        };
        look();
        timer = setInterval(look, 250);
        // After a restart the device usually comes back by itself to a page that may use it:
        // the person is asked only when it doesn't, a moment later.
        if (how.kind === 'sent' && link) setTimeout(() => { if (!done) on({ kind: 'prompt', prompt }); }, 1500);
        else on({ kind: 'prompt', prompt });
      });
    },
    // The prompt waiting for the person, if any.
    prompt: () => pending,
  };
}
